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
use aquilo_core::history::{self, Drop, ReadingRecord};
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
    /// First reading of the current fill cycle, kept apart from the history so
    /// trimming the history never moves the baseline the fill rate runs from.
    cycle_start: Option<ReadingRecord>,
    /// Latest known fill rate (cm/day). Carries over a pump-out, projecting the
    /// new cycle until it is old enough to yield a rate of its own.
    fill_rate: Option<f64>,
    /// Device counter of the last applied reading. The device publishes `/read`
    /// retained, so every (re)subscribe re-delivers the last reading.
    last_read_no: Option<i64>,
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
                cycle_start: p.cycle_start,
                fill_rate: p.fill_rate,
                last_read_no: p.last_read_no,
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
        cycle_start: None,
        fill_rate: None,
        last_read_no: None,
    }
}

/// Snapshots the runtime into the store. Persistence failures are logged but not
/// fatal: a write error must not stop the device from getting its `/state`.
fn persist(store: &impl Store, cfg: &Config, rt: &Runtime) {
    let snapshot = PersistedState {
        history: rt.history.clone(),
        lst_empty: rt.lst_empty.clone(),
        cycle_start: rt.cycle_start.clone(),
        fill_rate: rt.fill_rate,
        last_read_no: rt.last_read_no,
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
    if rt.is_redelivery(reading) {
        debug!(read_no = ?reading.read_no, "ignoring re-delivered reading");
        return Ok(());
    }
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
    /// Whether this is the last applied reading again (same device counter).
    fn is_redelivery(&self, reading: &Reading) -> bool {
        reading.read_no.is_some() && reading.read_no == self.last_read_no
    }

    /// Pure state transition for one reading: append it to history, settle
    /// whether the previous reading's sharp drop was a pump-out (starting a new
    /// fill cycle and advancing `lstEmpty`) or a glitch (dropped), trim the
    /// history to its cap, reproject `daysLeft` from the cycle's fill rate, and
    /// recompute the current `/state`. Kept free of I/O so it is unit-testable
    /// without the broker.
    fn apply_reading(
        &mut self,
        cfg: &Config,
        battery: &BatteryCurve,
        reading: &Reading,
        now: String,
    ) {
        self.last_read_no = reading.read_no;
        self.history.push(ReadingRecord {
            ts: now.clone(),
            lvl: reading.level_cm,
            bat: battery.percent(reading.battery_mv),
        });
        self.resolve_drop(cfg);
        if self.history.len() > cfg.history_max_len {
            let overflow = self.history.len() - cfg.history_max_len;
            self.history.drain(0..overflow);
        }

        let cycle_start = self
            .cycle_start
            .get_or_insert_with(|| self.history[0].clone());
        if let Some(rate) = history::fill_rate(cycle_start, &self.history) {
            self.fill_rate = Some(rate);
        }
        // Keep the prior estimate until any fill rate is known.
        let days_left = self
            .fill_rate
            .and_then(|rate| history::days_left(&self.history, cfg.calibration.full_dist, rate))
            .unwrap_or(self.current.days_left);

        self.current = SensorState::compute(
            reading,
            &cfg.calibration,
            battery,
            &statics(cfg, self.lst_empty.clone(), days_left),
            now,
        );
    }

    /// A sharp fullness drop is only judged once the reading after it arrives:
    /// if the level stays down the tank was emptied, if it bounces back the low
    /// reading was a radar glitch and must not reset the fill cycle.
    fn resolve_drop(&mut self, cfg: &Config) {
        let n = self.history.len();
        if n < 3 {
            return;
        }
        let suspect = self.history[n - 2].clone();
        match history::classify_drop(
            &self.history[n - 3],
            &suspect,
            &self.history[n - 1],
            &cfg.calibration,
            cfg.pump_out_drop_pct,
        ) {
            Drop::None => {}
            Drop::Outlier => {
                warn!(lvl = suspect.lvl, ts = %suspect.ts, "discarding glitch reading");
                self.history.remove(n - 2);
            }
            Drop::PumpOut => {
                info!(lst_empty = %suspect.ts, "pump-out detected; starting a new fill cycle");
                self.lst_empty = suspect.ts.clone();
                self.cycle_start = Some(suspect);
                self.history.drain(..n - 2);
            }
        }
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
    let looks_like_version = !text.is_empty()
        && text.len() < 64
        && text.contains('.')
        && !text.contains([' ', '{', ',']);
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
                cycle_start: None,
                fill_rate: Some(2.5),
                last_read_no: Some(73045),
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

    fn feed(rt: &mut Runtime, c: &Config, readings: &[(&str, i64)]) {
        for (ts, read1) in readings {
            rt.apply_reading(c, &BatteryCurve::default(), &reading(*read1), (*ts).into());
        }
    }

    #[test]
    fn a_retained_reading_redelivered_on_reconnect_is_recognised() {
        let dir = tempfile::tempdir().unwrap();
        let c = cfg(dir.path().to_str().unwrap());
        let mut rt = seed_runtime(&c);
        let numbered = |read_no: i64| {
            let payload = format!(
                r#"{{"sensor":"ae5058","read1":15000,"battery":"3770","readNo":{read_no}}}"#
            );
            Reading::parse(payload.as_bytes()).unwrap()
        };

        assert!(!rt.is_redelivery(&numbered(100)));
        rt.apply_reading(
            &c,
            &BatteryCurve::default(),
            &numbered(100),
            "2026-06-08T00:00:00+02:00".into(),
        );
        assert!(rt.is_redelivery(&numbered(100)));
        assert!(!rt.is_redelivery(&numbered(101)));
    }

    #[test]
    fn a_pump_out_is_confirmed_by_the_next_reading() {
        let dir = tempfile::tempdir().unwrap();
        let c = cfg(dir.path().to_str().unwrap());
        let mut rt = seed_runtime(&c);
        let seeded_empty = rt.lst_empty.clone();

        // Nearly full (50 cm ≈ 93%), then a near-empty reading (170 cm ≈ 6%).
        feed(
            &mut rt,
            &c,
            &[
                ("2026-06-08T00:00:00+02:00", 5000),
                ("2026-06-08T12:00:00+02:00", 5000),
                ("2026-06-09T00:00:00+02:00", 17000),
            ],
        );
        assert_eq!(
            rt.lst_empty, seeded_empty,
            "one low reading isn't proof yet"
        );

        feed(&mut rt, &c, &[("2026-06-09T06:00:00+02:00", 16950)]);
        assert_eq!(rt.lst_empty, "2026-06-09T00:00:00+02:00");
        assert_eq!(rt.current.lst_empty, "2026-06-09T00:00:00+02:00");
        assert_eq!(rt.history.len(), 2, "the old cycle is dropped");
    }

    #[test]
    fn a_single_glitch_neither_resets_the_cycle_nor_stays_in_history() {
        let dir = tempfile::tempdir().unwrap();
        let c = cfg(dir.path().to_str().unwrap());
        let mut rt = seed_runtime(&c);
        let seeded_empty = rt.lst_empty.clone();

        feed(
            &mut rt,
            &c,
            &[
                ("2026-06-08T00:00:00+02:00", 5000),
                ("2026-06-08T12:00:00+02:00", 17000),
                ("2026-06-09T00:00:00+02:00", 4990),
            ],
        );
        assert_eq!(rt.lst_empty, seeded_empty);
        assert!(rt.history.iter().all(|r| r.lvl < 60.0));
    }

    #[test]
    fn days_left_follows_the_cycle_and_survives_a_pump_out() {
        let dir = tempfile::tempdir().unwrap();
        let c = cfg(dir.path().to_str().unwrap());
        let mut rt = seed_runtime(&c);

        // A cycle filling 2 cm/day from 170 cm, one reading a day for 20 days.
        let cycle: Vec<(String, i64)> = (0..20)
            .map(|d| {
                (
                    format!("2026-06-{:02}T00:00:00+02:00", d + 1),
                    17000 - 200 * d,
                )
            })
            .collect();
        for (ts, read1) in &cycle {
            feed(&mut rt, &c, &[(ts, *read1)]);
        }
        // 132 cm smoothed, 92 cm to full at 2 cm/day.
        assert_eq!(rt.current.days_left, 46);

        // Emptied; the young cycle reuses the previous 2 cm/day: 138 cm to full.
        feed(
            &mut rt,
            &c,
            &[
                ("2026-06-21T00:00:00+02:00", 17800),
                ("2026-06-21T06:00:00+02:00", 17800),
            ],
        );
        assert_eq!(rt.lst_empty, "2026-06-21T00:00:00+02:00");
        assert_eq!(rt.current.days_left, 69);
    }
}
