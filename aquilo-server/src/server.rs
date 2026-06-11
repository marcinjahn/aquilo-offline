//! The `serve` loop: stand up the broker, connect as an MQTT client, seed the
//! retained connect-time messages, keep the device alive with `/ping`, and on each
//! raw reading recompute and republish a retained `/state`.
//!
//! State is durable. On startup the persisted store is loaded and the retained
//! `/state` is re-seeded from the last computed state before the device
//! reconnects, so a host reboot leaves a valid state immediately. Each new reading
//! is appended to history; `lstEmpty` (pump-out) and `daysLeft` (fill rate) are
//! recomputed over that history and the whole lot is persisted atomically.

use std::time::Duration;

use anyhow::Result;
use aquilo_core::history::{self, ReadingRecord};
use aquilo_core::{BatteryCurve, Reading, SensorState, StaticFields};
use aquilo_store::{JsonFileStore, PersistedState, Store};
use rumqttc::{AsyncClient, Event, MqttOptions, Packet, Publish, QoS};
use serde_json::json;
use tracing::{debug, info, warn};

use crate::broker;
use crate::clock;
use crate::config::Config;
use crate::topics::Topics;

/// Resolved broker connection for the `serve` client, from either the configured
/// external broker or the in-process one.
struct BrokerConn {
    host: String,
    port: u16,
    user: String,
    pass: String,
}

/// The evolving server state. Only the serve loop touches it.
struct Runtime {
    /// The current computed `/state` (seeded, then updated on each reading).
    current: SensorState,
    /// Rolling reading history backing the `daysLeft` projection.
    history: Vec<ReadingRecord>,
    /// The `lstEmpty` baseline, advanced when a pump-out is detected.
    lst_empty: String,
}

pub async fn run(cfg: Config) -> Result<()> {
    // Two deployments share this loop: with an external broker configured (the HA
    // Mosquitto add-on) we connect as a plain client; otherwise we run our own
    // in-process broker and connect to it on loopback (standalone Docker / dev).
    let conn = match &cfg.broker {
        Some(b) => {
            info!(host = %b.host, port = b.port, "connecting to external broker (no embedded broker)");
            BrokerConn {
                host: b.host.clone(),
                port: b.port,
                user: b.username.clone(),
                pass: b.password.clone(),
            }
        }
        None => {
            broker::spawn(&cfg)?;
            BrokerConn {
                host: "127.0.0.1".to_string(),
                port: cfg.listen_port,
                user: broker::INTERNAL_USER.to_string(),
                pass: broker::INTERNAL_PASS.to_string(),
            }
        }
    };

    let topics = Topics::new(&cfg.receiver_id);
    let battery = BatteryCurve::default();
    let store = JsonFileStore::in_dir(&cfg.data_dir);

    let mut rt = load_runtime(&cfg, &store);
    // Persist immediately so the store reflects the seed (and exists) even before
    // the first live reading arrives.
    persist(&store, &cfg, &rt);

    let mut opts = MqttOptions::new(
        format!("aquilo-server-{}", cfg.receiver_id),
        conn.host,
        conn.port,
    );
    opts.set_credentials(conn.user, conn.pass);
    opts.set_keep_alive(Duration::from_secs(30));

    let (client, mut eventloop) = AsyncClient::new(opts, 64);

    spawn_ping(client.clone(), topics.ping.clone(), cfg.ping_interval_secs);

    loop {
        match eventloop.poll().await {
            Ok(Event::Incoming(Packet::ConnAck(_))) => {
                info!("connected to broker; seeding retained messages");
                // Re-seed on every (re)connect so the retained set is restored
                // even after a broker restart, which clears its in-memory store.
                seed_retained(&client, &cfg, &topics, &rt.current).await?;
                for topic in [&topics.read, &topics.log, &topics.connection] {
                    client.subscribe(topic, QoS::AtMostOnce).await?;
                }
                info!("subscribed to device topics");
            }
            Ok(Event::Incoming(Packet::Publish(p))) => {
                handle_publish(&client, &cfg, &topics, &battery, &store, &mut rt, &p).await?;
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

/// Builds the initial [`Runtime`] from the store, falling back to the config seed
/// on a first run or an unreadable store. A persisted `last_state` is re-seeded
/// verbatim so the device sees the exact state it last had after a reboot.
fn load_runtime(cfg: &Config, store: &impl Store) -> Runtime {
    match store.load() {
        Ok(Some(p)) => {
            info!(
                history = p.history.len(),
                lst_empty = %p.lst_empty,
                "loaded persisted state"
            );
            let lst_empty = if p.lst_empty.is_empty() {
                cfg.state.lst_empty.clone()
            } else {
                p.lst_empty
            };
            let current = p
                .last_state
                .unwrap_or_else(|| seed_state(cfg, clock::now_rfc3339()));
            Runtime {
                current,
                history: p.history,
                lst_empty,
            }
        }
        Ok(None) => {
            info!("no persisted state; seeding from config");
            seed_runtime(cfg)
        }
        Err(e) => {
            warn!(error = %e, "failed to load persisted state; seeding from config");
            seed_runtime(cfg)
        }
    }
}

fn seed_runtime(cfg: &Config) -> Runtime {
    Runtime {
        current: seed_state(cfg, clock::now_rfc3339()),
        history: Vec::new(),
        lst_empty: cfg.state.lst_empty.clone(),
    }
}

/// Snapshots the runtime into the store. Persistence failures are logged but not
/// fatal: a write error must not stop the device from getting its `/state`.
fn persist(store: &impl Store, cfg: &Config, rt: &Runtime) {
    let snapshot = PersistedState {
        history: rt.history.clone(),
        lst_empty: rt.lst_empty.clone(),
        last_state: Some(rt.current.clone()),
        calibration: cfg.calibration,
    };
    if let Err(e) = store.save(&snapshot) {
        warn!(error = %e, "failed to persist state");
    }
}

/// Identity + `from` carried into every computed reading. The history-backed
/// fields (`lstEmpty`/`daysLeft`) are supplied per-reading by the serve loop.
fn statics(cfg: &Config, lst_empty: String, days_left: i64) -> StaticFields {
    StaticFields {
        sensor_id: cfg.sensor_id.clone(),
        name: cfg.sensor_name.clone(),
        lst_empty,
        days_left,
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

#[allow(clippy::too_many_arguments)]
async fn handle_publish(
    client: &AsyncClient,
    cfg: &Config,
    topics: &Topics,
    battery: &BatteryCurve,
    store: &impl Store,
    rt: &mut Runtime,
    p: &Publish,
) -> Result<()> {
    let topic = p.topic.as_str();

    if topic == topics.read {
        match Reading::parse(&p.payload) {
            Ok(reading) => {
                handle_read(client, cfg, topics, battery, store, rt, &reading).await?;
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

/// Folds a fresh reading into the runtime, then persists and republishes the
/// recomputed `/state`.
async fn handle_read(
    client: &AsyncClient,
    cfg: &Config,
    topics: &Topics,
    battery: &BatteryCurve,
    store: &impl Store,
    rt: &mut Runtime,
    reading: &Reading,
) -> Result<()> {
    rt.apply_reading(cfg, battery, reading, clock::now_rfc3339());
    info!(
        lvl = rt.current.lvl,
        pct = rt.current.pct,
        bat = rt.current.bat,
        days_left = rt.current.days_left,
        lvl_to_full = rt.current.lvl_to_full,
        sensor = %reading.sensor,
        "read received; republishing computed state"
    );

    persist(store, cfg, rt);
    publish_state(client, &topics.state, &rt.current).await?;
    Ok(())
}

impl Runtime {
    /// Pure state transition for one reading: detect a pump-out (advancing
    /// `lstEmpty`), append to history (trimming the oldest past the cap),
    /// reproject `daysLeft` from the fill rate, and recompute the current
    /// `/state`. Kept free of I/O so it is unit-testable without the broker.
    fn apply_reading(
        &mut self,
        cfg: &Config,
        battery: &BatteryCurve,
        reading: &Reading,
        now: String,
    ) {
        let bat = battery.percent(reading.battery_mv);
        let new_pct = cfg.calibration.pct(reading.level_cm);

        // A sharp fall in fullness versus the last state means the tank was
        // emptied; reset the pump-out baseline to this reading's time.
        if history::is_pump_out(self.current.pct, new_pct, cfg.pump_out_drop_pct) {
            info!(prev_pct = self.current.pct, new_pct, lst_empty = %now, "pump-out detected; resetting lstEmpty");
            self.lst_empty = now.clone();
        }

        self.history.push(ReadingRecord {
            ts: now.clone(),
            lvl: reading.level_cm,
            bat,
        });
        if self.history.len() > cfg.history_max_len {
            let overflow = self.history.len() - cfg.history_max_len;
            self.history.drain(0..overflow);
        }

        // Project days-to-full from the fill rate; keep the prior estimate when
        // the history can't yet support one.
        let days_left = history::days_left(&self.history, cfg.calibration.full_dist)
            .unwrap_or(self.current.days_left);

        self.current = SensorState::compute(
            reading,
            &cfg.calibration,
            battery,
            &statics(cfg, self.lst_empty.clone(), days_left),
            now,
        );
    }
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::StateSeed;
    use aquilo_core::Calibration;

    fn cfg(data_dir: &str) -> Config {
        Config {
            receiver_id: "ae83fc".into(),
            sensor_id: "ae5058".into(),
            mqtt_user: "ae83fc".into(),
            mqtt_pass: "48007129".into(),
            firmware_version: "1.7.1.9_sh_en".into(),
            sensor_name: "ae5058".into(),
            radar_skip: 9,
            radar_repeat: 9,
            bind_addr: "127.0.0.1".into(),
            listen_port: 1,
            ping_interval_secs: 1,
            data_dir: data_dir.into(),
            history_max_len: 500,
            pump_out_drop_pct: 25,
            calibration: Calibration::default(),
            broker: None,
            state: StateSeed {
                lvl: 150.2,
                pct: 20,
                bat: 83,
                days_left: 51,
                lvl_to_full: 110,
                lst_empty: "2026-05-30T00:17:19+02:00".into(),
                from: "node-4".into(),
            },
        }
    }

    fn reading(read1: i64) -> Reading {
        let payload = format!(r#"{{"sensor":"ae5058","read1":{read1},"battery":"3770"}}"#);
        Reading::parse(payload.as_bytes()).unwrap()
    }

    #[test]
    fn restart_reseeds_state_from_the_store_not_the_config_seed() {
        let dir = tempfile::tempdir().unwrap();
        let c = cfg(dir.path().to_str().unwrap());
        let store = JsonFileStore::in_dir(&c.data_dir);

        // A prior run that computed and persisted a 152.8 cm reading.
        let prior = SensorState::compute(
            &reading(15280),
            &c.calibration,
            &BatteryCurve::default(),
            &statics(&c, "2026-06-01T00:00:00+02:00".into(), 7),
            "2026-06-09T12:00:00+02:00".into(),
        );
        store
            .save(&PersistedState {
                history: vec![ReadingRecord {
                    ts: "2026-06-09T12:00:00+02:00".into(),
                    lvl: 152.8,
                    bat: 83,
                }],
                lst_empty: "2026-06-01T00:00:00+02:00".into(),
                last_state: Some(prior.clone()),
                calibration: c.calibration,
            })
            .unwrap();

        // The post-restart load re-seeds from the store: the /state it will
        // publish is the persisted one (152.8), not the config seed (150.2).
        let rt = load_runtime(&c, &store);
        assert_eq!(rt.current.to_json(), prior.to_json());
        assert_eq!(rt.current.lvl, 152.8);
        assert_ne!(rt.current.lvl, c.state.lvl);
        assert_eq!(rt.lst_empty, "2026-06-01T00:00:00+02:00");
        assert_eq!(rt.history.len(), 1);
    }

    #[test]
    fn a_reading_sequence_with_a_pump_out_updates_lst_empty() {
        let dir = tempfile::tempdir().unwrap();
        let c = cfg(dir.path().to_str().unwrap());
        let battery = BatteryCurve::default();
        let mut rt = seed_runtime(&c);
        let seeded_empty = rt.lst_empty.clone();

        // A nearly-full reading (50 cm ≈ 93%); fullness rising, so no pump-out.
        rt.apply_reading(&c, &battery, &reading(5000), "2026-06-08T00:00:00+02:00".into());
        assert_eq!(rt.lst_empty, seeded_empty, "rising level must not reset lstEmpty");

        // Then a near-empty reading (170 cm ≈ 6%): an ~87-point drop = pump-out.
        rt.apply_reading(&c, &battery, &reading(17000), "2026-06-09T00:00:00+02:00".into());
        assert_eq!(rt.lst_empty, "2026-06-09T00:00:00+02:00", "pump-out resets lstEmpty");
        assert_eq!(
            rt.current.lst_empty, "2026-06-09T00:00:00+02:00",
            "computed /state carries the new lstEmpty"
        );
    }
}
