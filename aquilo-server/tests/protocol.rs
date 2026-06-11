//! Protocol integration test: stand up the in-process broker + server, connect a
//! simulated device, and exercise CONNECT → subscribe → receive 3 retained →
//! publish `/read` → assert a retained `/state` reflecting the reading.

use std::collections::HashSet;
use std::time::{Duration, Instant};

use aquilo_server::config::{Config, StateSeed};
use aquilo_server::topics::Topics;
use rumqttc::{AsyncClient, Event, EventLoop, MqttOptions, Packet, Publish, QoS};

const TEST_PORT: u16 = 18883;

fn test_config() -> Config {
    Config {
        receiver_id: "ae83fc".to_string(),
        sensor_id: "ae5058".to_string(),
        mqtt_user: "ae83fc".to_string(),
        mqtt_pass: "48007129".to_string(),
        firmware_version: "1.7.1.9_sh_en".to_string(),
        sensor_name: "ae5058".to_string(),
        radar_skip: 9,
        radar_repeat: 9,
        bind_addr: "127.0.0.1".to_string(),
        listen_port: TEST_PORT,
        ping_interval_secs: 1,
        state: StateSeed {
            lvl: 150.2,
            pct: 20,
            bat: 83,
            days_left: 51,
            lvl_to_full: 110,
            lst_empty: "2026-05-30T00:17:19+02:00".to_string(),
            from: "node-4".to_string(),
        },
    }
}

fn device_client(client_id: &str) -> (AsyncClient, EventLoop) {
    let mut opts = MqttOptions::new(client_id, "127.0.0.1", TEST_PORT);
    opts.set_credentials("ae83fc", "48007129");
    opts.set_keep_alive(Duration::from_secs(30));
    AsyncClient::new(opts, 64)
}

/// Polls the event loop, collecting PUBLISH packets, until `done` is satisfied by
/// the accumulated set or the deadline passes.
async fn collect_until<F>(eventloop: &mut EventLoop, deadline: Duration, mut done: F) -> Vec<Publish>
where
    F: FnMut(&[Publish]) -> bool,
{
    let start = Instant::now();
    let mut publishes = Vec::new();
    while start.elapsed() < deadline {
        match tokio::time::timeout(Duration::from_millis(500), eventloop.poll()).await {
            Ok(Ok(Event::Incoming(Packet::Publish(p)))) => {
                publishes.push(p);
                if done(&publishes) {
                    break;
                }
            }
            Ok(Ok(_)) => {}
            Ok(Err(_)) => tokio::time::sleep(Duration::from_millis(100)).await,
            Err(_) => {} // poll timed out; keep waiting until the deadline
        }
    }
    publishes
}

fn sensor_lvl(p: &Publish) -> Option<f64> {
    let value: serde_json::Value = serde_json::from_slice(&p.payload).ok()?;
    value["sensors"][0]["lvl"].as_f64()
}

#[tokio::test]
async fn device_handshake_and_read_roundtrip() {
    let cfg = test_config();
    let topics = Topics::new(&cfg.receiver_id);

    tokio::spawn({
        let cfg = cfg.clone();
        async move {
            let _ = aquilo_server::server::run(cfg).await;
        }
    });

    // Let the broker bind and the server seed the retained messages.
    tokio::time::sleep(Duration::from_millis(1500)).await;

    // --- device connects and subscribes to the 3 connect-time topics ---
    let (device, mut eventloop) = device_client("CieczSensorae83fc");
    for topic in [&topics.version, &topics.state, &topics.radar_params] {
        device.subscribe(topic, QoS::AtMostOnce).await.unwrap();
    }

    let retained = collect_until(&mut eventloop, Duration::from_secs(10), |ps| {
        let topics_seen: HashSet<&str> = ps.iter().map(|p| p.topic.as_str()).collect();
        topics_seen.len() >= 3
    })
    .await;

    let seen: HashSet<&str> = retained.iter().map(|p| p.topic.as_str()).collect();
    assert!(
        seen.contains(topics.version.as_str()),
        "expected retained version, got {seen:?}"
    );
    assert!(
        seen.contains(topics.state.as_str()),
        "expected retained state, got {seen:?}"
    );
    assert!(
        seen.contains(topics.radar_params.as_str()),
        "expected retained radarParams, got {seen:?}"
    );

    // All connect-time messages must arrive with the retain bit set.
    for p in &retained {
        assert!(p.retain, "message on {} was not retained", p.topic);
    }

    let version = retained
        .iter()
        .find(|p| p.topic == topics.version)
        .unwrap();
    assert_eq!(
        String::from_utf8_lossy(&version.payload),
        "1.7.1.9_sh_en",
        "firmware version must be echoed verbatim to avoid OTA"
    );

    let seed_state = retained.iter().find(|p| p.topic == topics.state).unwrap();
    assert_eq!(sensor_lvl(seed_state), Some(150.2), "seed state lvl");

    // --- device publishes a raw reading; lvl 152.8 = read1 15280 ---
    let read_payload = serde_json::json!({
        "salt": "abc123",
        "raw": "_e23_ae5058_1528_3770_59_0_73045_72976_1_2_1_0_0_0",
        "v": "e23",
        "sensor": "ae5058",
        "read1": 15280,
        "battery": "3770",
        "crc": true,
        "readNo": 73045,
    });
    device
        .publish(
            &topics.read,
            QoS::AtMostOnce,
            false,
            serde_json::to_vec(&read_payload).unwrap(),
        )
        .await
        .unwrap();

    // The live subscriber sees the recomputed state forwarded.
    let updates = collect_until(&mut eventloop, Duration::from_secs(10), |ps| {
        ps.iter()
            .any(|p| p.topic == topics.state && sensor_lvl(p) == Some(152.8))
    })
    .await;
    assert!(
        updates
            .iter()
            .any(|p| p.topic == topics.state && sensor_lvl(p) == Some(152.8)),
        "expected a /state update with lvl 152.8, got {:?}",
        updates
            .iter()
            .map(|p| (p.topic.clone(), sensor_lvl(p)))
            .collect::<Vec<_>>()
    );

    // A fresh subscriber proves the new state was stored *retained*, not just
    // forwarded to the already-connected device.
    let (fresh, mut fresh_loop) = device_client("CieczSensorae83fc-probe");
    fresh.subscribe(&topics.state, QoS::AtMostOnce).await.unwrap();
    let retained_after = collect_until(&mut fresh_loop, Duration::from_secs(10), |ps| {
        ps.iter().any(|p| p.topic == topics.state)
    })
    .await;
    let state_msg = retained_after
        .iter()
        .find(|p| p.topic == topics.state)
        .expect("fresh subscriber should receive a retained /state");
    assert!(state_msg.retain, "republished /state must be retained");
    assert_eq!(
        sensor_lvl(state_msg),
        Some(152.8),
        "retained /state must reflect the latest reading"
    );
}
