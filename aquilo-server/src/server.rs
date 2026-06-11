//! The `serve` loop: stand up the broker, connect as an MQTT client, seed the
//! retained connect-time messages, keep the device alive with `/ping`, and on each
//! raw reading republish a retained `/state`.

use std::time::Duration;

use anyhow::Result;
use aquilo_core::{BatteryCurve, Reading, SensorState, StaticFields};
use rumqttc::{AsyncClient, Event, MqttOptions, Packet, Publish, QoS};
use serde_json::json;
use tracing::{debug, info, warn};

use crate::broker;
use crate::clock;
use crate::config::Config;
use crate::topics::Topics;

pub async fn run(cfg: Config) -> Result<()> {
    broker::spawn(&cfg)?;
    let topics = Topics::new(&cfg.receiver_id);
    let battery = BatteryCurve::default();

    let mut opts = MqttOptions::new(
        format!("aquilo-server-{}", cfg.receiver_id),
        "127.0.0.1",
        cfg.listen_port,
    );
    opts.set_credentials(broker::INTERNAL_USER, broker::INTERNAL_PASS);
    opts.set_keep_alive(Duration::from_secs(30));

    let (client, mut eventloop) = AsyncClient::new(opts, 64);

    spawn_ping(client.clone(), topics.ping.clone(), cfg.ping_interval_secs);

    // The state evolves with each reading; only this loop touches it.
    let mut current = seed_state(&cfg, clock::now_rfc3339());

    loop {
        match eventloop.poll().await {
            Ok(Event::Incoming(Packet::ConnAck(_))) => {
                info!("connected to broker; seeding retained messages");
                // Re-seed on every (re)connect so the retained set is restored
                // even after a broker restart, which clears its in-memory store.
                seed_retained(&client, &cfg, &topics, &current).await?;
                for topic in [&topics.read, &topics.log, &topics.connection] {
                    client.subscribe(topic, QoS::AtMostOnce).await?;
                }
                info!("subscribed to device topics");
            }
            Ok(Event::Incoming(Packet::Publish(p))) => {
                handle_publish(&client, &cfg, &topics, &battery, &mut current, &p).await?;
            }
            Ok(_) => {}
            Err(e) => {
                // The event loop reconnects on the next poll; back off to avoid a
                // busy spin while the broker is still coming up.
                warn!(error = %e, "mqtt connection error; retrying");
                tokio::time::sleep(Duration::from_secs(1)).await;
            }
        }
    }
}

fn spawn_ping(client: AsyncClient, topic: String, interval_secs: u64) {
    tokio::spawn(async move {
        let mut tick = tokio::time::interval(Duration::from_secs(interval_secs));
        loop {
            tick.tick().await;
            match client.publish(&topic, QoS::AtMostOnce, false, "ping").await {
                Ok(()) => debug!(%topic, "published /ping"),
                Err(e) => warn!(error = %e, "failed to publish /ping"),
            }
        }
    });
}

/// Identity + history-backed fields carried into every computed reading. Until
/// persistence lands, `lstEmpty`/`daysLeft` come straight from the config seed.
fn statics(cfg: &Config) -> StaticFields {
    StaticFields {
        sensor_id: cfg.sensor_id.clone(),
        name: cfg.sensor_name.clone(),
        lst_empty: cfg.state.lst_empty.clone(),
        days_left: cfg.state.days_left,
        from: cfg.state.from.clone(),
    }
}

/// The last-known `/state` to serve before the first live reading, taken verbatim
/// from the config seed (a captured vendor state). The live compute path takes
/// over on the first `/read`.
fn seed_state(cfg: &Config, lst_read: String) -> SensorState {
    let s = &cfg.state;
    SensorState {
        id: cfg.sensor_id.clone(),
        name: cfg.sensor_name.clone(),
        lvl: s.lvl,
        pct: s.pct,
        bat: s.bat,
        lst_read,
        lst_empty: s.lst_empty.clone(),
        days_left: s.days_left,
        lvl_to_full: s.lvl_to_full,
        from: s.from.clone(),
    }
}

async fn seed_retained(
    client: &AsyncClient,
    cfg: &Config,
    topics: &Topics,
    state: &SensorState,
) -> Result<()> {
    client
        .publish(
            &topics.version,
            QoS::AtLeastOnce,
            true,
            cfg.firmware_version.clone(),
        )
        .await?;

    let radar = json!({
        "sensor": cfg.sensor_id,
        "skip": cfg.radar_skip,
        "repeat": cfg.radar_repeat,
    });
    client
        .publish(
            &topics.radar_params,
            QoS::AtLeastOnce,
            true,
            serde_json::to_vec(&radar)?,
        )
        .await?;

    publish_state(client, &topics.state, state).await?;
    info!(version = %cfg.firmware_version, "seeded retained: version, radarParams, state");
    Ok(())
}

async fn publish_state(client: &AsyncClient, state_topic: &str, state: &SensorState) -> Result<()> {
    let payload = serde_json::to_vec(&state.to_json())?;
    client
        .publish(state_topic, QoS::AtLeastOnce, true, payload)
        .await?;
    info!(lvl = state.lvl, lst_read = %state.lst_read, "published retained /state");
    Ok(())
}

async fn handle_publish(
    client: &AsyncClient,
    cfg: &Config,
    topics: &Topics,
    battery: &BatteryCurve,
    state: &mut SensorState,
    p: &Publish,
) -> Result<()> {
    let topic = p.topic.as_str();

    if topic == topics.read {
        match Reading::parse(&p.payload) {
            Ok(reading) => {
                *state = SensorState::compute(
                    &reading,
                    &cfg.calibration,
                    battery,
                    &statics(cfg),
                    clock::now_rfc3339(),
                );
                info!(
                    lvl = state.lvl,
                    pct = state.pct,
                    bat = state.bat,
                    lvl_to_full = state.lvl_to_full,
                    sensor = %reading.sensor,
                    "read received; republishing computed state"
                );
                publish_state(client, &topics.state, state).await?;
            }
            Err(e) => warn!(error = %e, "failed to parse read payload"),
        }
    } else if topic == topics.log {
        let text = String::from_utf8_lossy(&p.payload);
        info!(%text, "device log");
        // Echo back whatever firmware the device reports so it never sees a newer
        // version and triggers an OTA.
        if let Some(fw) = parse_firmware(&p.payload) {
            if fw != cfg.firmware_version {
                info!(reported = %fw, configured = %cfg.firmware_version, "echoing device-reported firmware version");
            }
            client
                .publish(&topics.version, QoS::AtLeastOnce, true, fw)
                .await?;
        }
    } else if topic == topics.connection {
        info!(status = %String::from_utf8_lossy(&p.payload), "device connection status");
    } else {
        debug!(%topic, "ignoring publish");
    }

    Ok(())
}

/// Best-effort extraction of a firmware version from the device's `/log` payload.
/// The exact format isn't pinned down, so we try JSON fields first, then fall back
/// to a bare version-looking token; otherwise we keep the configured seed.
fn parse_firmware(payload: &[u8]) -> Option<String> {
    if let Ok(value) = serde_json::from_slice::<serde_json::Value>(payload) {
        for key in ["v", "version", "fw", "ver"] {
            if let Some(s) = value.get(key).and_then(|x| x.as_str()) {
                return Some(s.to_string());
            }
        }
    }

    let text = std::str::from_utf8(payload).ok()?.trim();
    let looks_like_version =
        !text.is_empty() && text.len() < 64 && text.contains('.') && !text.contains([' ', '{', ',']);
    looks_like_version.then(|| text.to_string())
}
