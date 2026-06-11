//! Onboarding integration test: drive the cloud-free `observe` broker with a
//! simulated device (a real `rumqttc` client) and assert it reconstructs a usable
//! config + initial state from the live connection alone — no cloud reachable.

use std::time::{Duration, Instant};

use aquilo_server::config::Config;
use aquilo_server::mqtt_wire;
use aquilo_server::onboard::{self, LearnOptions, OnboardOptions};
use aquilo_store::{JsonFileStore, Store};
use rumqttc::{AsyncClient, MqttOptions, QoS};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

const TEST_PORT: u16 = 18884;
const LEARN_PORT: u16 = 18885;
const CLOUD_PORT: u16 = 18886;

#[tokio::test]
async fn observe_reconstructs_config_from_a_live_device_with_no_cloud() {
    let dir = tempfile::tempdir().unwrap();
    let cfg_path = dir.path().join("config.toml");
    let opts = OnboardOptions {
        bind_addr: "127.0.0.1".to_string(),
        listen_port: TEST_PORT,
        out_config: cfg_path.to_str().unwrap().to_string(),
        data_dir: dir.path().to_str().unwrap().to_string(),
    };

    let observe = tokio::spawn(onboard::run_observe(opts));
    tokio::time::sleep(Duration::from_millis(300)).await; // let it bind

    // The device: fixed firmware clientId/user/pass it cannot change.
    let mut mqtt = MqttOptions::new("CieczSensorae83fc", "127.0.0.1", TEST_PORT);
    mqtt.set_credentials("ae83fc", "48007129");
    mqtt.set_keep_alive(Duration::from_secs(5));
    let (device, mut eventloop) = AsyncClient::new(mqtt, 64);

    // Drive the device's event loop (handshake, keepalive, sending publishes).
    let pump = tokio::spawn(async move {
        loop {
            if eventloop.poll().await.is_err() {
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
        }
    });

    // Subscribe to the connect-time topics, so observe seeds the retained set.
    for topic in [
        "/version/czujnik_szamba/ae83fc",
        "/users/ae83fc/state",
        "/users/ae83fc/receivers/ae83fc/radarParams",
    ] {
        device.subscribe(topic, QoS::AtMostOnce).await.unwrap();
    }

    // Announce firmware on /log, then publish a raw reading on /read.
    device
        .publish(
            "/users/ae83fc/sensors/ae83fc/log",
            QoS::AtMostOnce,
            false,
            "1.7.1.9_sh_en D8:BC:38:AE:83:FC".as_bytes().to_vec(),
        )
        .await
        .unwrap();
    device
        .publish(
            "/users/ae83fc/sensors/ae83fc/read",
            QoS::AtMostOnce,
            false,
            br#"{"sensor":"ae5058","read1":15280,"battery":"3770","raw":"_e23_ae5058_1528_3770_59_0_73045_72976_1"}"#.to_vec(),
        )
        .await
        .unwrap();

    // Wait for observe to recover everything and write the config.
    let start = Instant::now();
    while start.elapsed() < Duration::from_secs(10) {
        if cfg_path.exists() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert!(cfg_path.exists(), "observe should have written a config");

    // AC: receiver_id, sensor_id, clientId-derived user/pass and firmware come
    // purely from the device; serve consumes the config with no hand-editing.
    let cfg = Config::load(opts_path(&cfg_path)).expect("generated config must load");
    assert_eq!(cfg.receiver_id, "ae83fc");
    assert_eq!(cfg.sensor_id, "ae5058");
    assert_eq!(cfg.mqtt_user, "ae83fc");
    assert_eq!(cfg.mqtt_pass, "48007129");
    assert_eq!(cfg.firmware_version, "1.7.1.9_sh_en");
    // radarParams + calibration are documented defaults.
    assert_eq!(cfg.radar_skip, 9);
    assert_eq!(cfg.calibration.full_dist, 40.0);
    // The seed reflects the captured reading (152.8 cm → pct 18, lvlToFull 113).
    assert_eq!(cfg.state.lvl, 152.8);
    assert_eq!(cfg.state.pct, 18);
    assert_eq!(cfg.state.lvl_to_full, 113);

    // The initial state file is what serve re-seeds the retained /state from.
    let persisted = JsonFileStore::in_dir(dir.path()).load().unwrap().unwrap();
    let last = persisted.last_state.unwrap();
    assert_eq!(last.id, "ae5058");
    assert_eq!(last.lvl, 152.8);
    assert_eq!(last.pct, 18);

    pump.abort();
    observe.abort();
}

fn opts_path(p: &std::path::Path) -> &str {
    p.to_str().unwrap()
}

/// A stand-in vendor cloud: accept the proxied connection, drain whatever the
/// device sends, and push the three retained connect-time messages the real cloud
/// would — so `learn` can tee and record them.
async fn fake_cloud(listener: TcpListener) {
    loop {
        let Ok((mut sock, _)) = listener.accept().await else {
            return;
        };
        tokio::spawn(async move {
            sock.set_nodelay(true).ok();
            // Accept the connection and deliver the retained set the real broker
            // sends on connect.
            sock.write_all(&mqtt_wire::connack()).await.ok();
            sock.write_all(&mqtt_wire::publish(
                "/version/czujnik_szamba/ae83fc",
                b"1.7.1.9_sh_en",
                true,
            ))
            .await
            .ok();
            sock.write_all(&mqtt_wire::publish(
                "/users/ae83fc/receivers/ae83fc/radarParams",
                br#"{"sensor":"ae5058","skip":9,"repeat":9,"from":"ovh4-docker"}"#,
                true,
            ))
            .await
            .ok();
            sock.write_all(&mqtt_wire::publish(
                "/users/ae83fc/state",
                br#"{"sensors":[{"id":"ae5058","lvl":150.2,"pct":20,"bat":83,"lstRead":"2026-06-10T20:44:35+02:00","lstEmpty":"2026-05-30T00:17:19+02:00","daysLeft":51,"name":"ae5058","lvlToFull":110}],"from":"node-4"}"#,
                true,
            ))
            .await
            .ok();
            // Keep the connection open and drain the device's bytes.
            let mut buf = [0u8; 1024];
            while let Ok(n) = sock.read(&mut buf).await {
                if n == 0 {
                    break;
                }
            }
        });
    }
}

#[tokio::test]
async fn learn_proxies_to_the_cloud_and_records_the_handshake() {
    let dir = tempfile::tempdir().unwrap();
    let cfg_path = dir.path().join("config.toml");

    let cloud = TcpListener::bind(("127.0.0.1", CLOUD_PORT)).await.unwrap();
    tokio::spawn(fake_cloud(cloud));

    let learn = tokio::spawn(onboard::run_learn(LearnOptions {
        onboard: OnboardOptions {
            bind_addr: "127.0.0.1".to_string(),
            listen_port: LEARN_PORT,
            out_config: cfg_path.to_str().unwrap().to_string(),
            data_dir: dir.path().to_str().unwrap().to_string(),
        },
        upstream_host: "127.0.0.1".to_string(),
        upstream_port: CLOUD_PORT,
    }));
    tokio::time::sleep(Duration::from_millis(300)).await;

    // The device connects *through* learn to the (fake) cloud.
    let mut mqtt = MqttOptions::new("CieczSensorae83fc", "127.0.0.1", LEARN_PORT);
    mqtt.set_credentials("ae83fc", "48007129");
    mqtt.set_keep_alive(Duration::from_secs(5));
    let (_device, mut eventloop) = AsyncClient::new(mqtt, 64);
    let pump = tokio::spawn(async move {
        loop {
            if eventloop.poll().await.is_err() {
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
        }
    });

    let start = Instant::now();
    while start.elapsed() < Duration::from_secs(10) {
        if cfg_path.exists() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert!(cfg_path.exists(), "learn should have written a config");

    let cfg = Config::load(opts_path(&cfg_path)).expect("generated config must load");
    // Credentials from the device CONNECT; identity + seed from the cloud's
    // retained /state and radarParams.
    assert_eq!(cfg.receiver_id, "ae83fc");
    assert_eq!(cfg.mqtt_pass, "48007129");
    assert_eq!(cfg.sensor_id, "ae5058");
    assert_eq!(cfg.firmware_version, "1.7.1.9_sh_en");
    assert_eq!(cfg.state.lvl, 150.2);
    assert_eq!(cfg.state.days_left, 51);
    assert_eq!(cfg.state.lst_empty, "2026-05-30T00:17:19+02:00");

    let persisted = JsonFileStore::in_dir(dir.path()).load().unwrap().unwrap();
    assert_eq!(persisted.last_state.unwrap().lvl, 150.2);

    pump.abort();
    learn.abort();
}
