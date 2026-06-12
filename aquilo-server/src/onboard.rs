//! `learn` and `observe` onboarding modes: recover a device's identity and write
//! the `serve` config + initial state file, so a new user never hand-edits IDs,
//! topics, or credentials (PRD: Onboarding & recovery modes).
//!
//! - **`learn`** is a transparent MQTT proxy to the real vendor cloud (reached by
//!   IP, to dodge the DNS-rewrite loop). It forwards bytes verbatim and tees both
//!   directions through the wire [`Decoder`], recording the device's CONNECT
//!   credentials and reported firmware plus the cloud's retained connect-time
//!   messages — the most faithful possible config and state seed.
//! - **`observe`** needs no cloud. It is a tiny stand-in broker that completes the
//!   handshake itself and seeds the retained messages from documented defaults to
//!   coax the device into reporting a reading, then recovers the same identity
//!   purely from what the device announces on a live connection.
//!
//! Both funnel into a single [`Facts`] collector and the same writers, so the two
//! modes produce identically-shaped output; only the source of the facts differs.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use anyhow::{Context, Result};
use aquilo_core::{BatteryCurve, Calibration, Reading, SensorState};
use aquilo_store::{JsonFileStore, PersistedState, Store};
use serde_json::json;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tracing::{info, warn};

use crate::clock;
use crate::mqtt_wire::{self, Decoder, Packet};

/// The prefix the firmware prepends to the receiver id to form its MQTT clientId.
const CLIENT_ID_PREFIX: &str = "CieczSensor";
/// mqtt.aquilo.cloud, by IP on purpose so the proxy doesn't loop back through the
/// DNS rewrite the user keeps pointed at this host.
pub const DEFAULT_UPSTREAM_HOST: &str = "57.128.198.238";

/// Where the onboarding output goes and how the capture socket is bound. Shared by
/// both modes.
#[derive(Clone, Debug)]
pub struct OnboardOptions {
    pub bind_addr: String,
    pub listen_port: u16,
    /// Path the generated `config.toml` is written to.
    pub out_config: String,
    /// Directory the initial `state.json` is written to (becomes `data_dir`).
    pub data_dir: String,
}

#[derive(Clone, Debug)]
pub struct LearnOptions {
    pub onboard: OnboardOptions,
    pub upstream_host: String,
    pub upstream_port: u16,
}

/// The `/state` seed values recovered for the initial retained message.
#[derive(Clone, Debug)]
struct Seed {
    lvl: f64,
    pct: i64,
    bat: i64,
    days_left: i64,
    lvl_to_full: i64,
    /// Last pump-out (RFC3339). Empty when unknown (`observe`): it self-heals.
    lst_empty: String,
    from: String,
}

/// Device identity assembled from whatever the device and/or cloud reveal. Fields
/// fill in as packets arrive; [`Facts::is_complete`] gates writing the config.
#[derive(Clone, Debug, Default)]
pub struct Facts {
    receiver_id: Option<String>,
    client_id: Option<String>,
    mqtt_user: Option<String>,
    mqtt_pass: Option<String>,
    firmware: Option<String>,
    sensor_id: Option<String>,
    sensor_name: Option<String>,
    radar_skip: Option<i64>,
    radar_repeat: Option<i64>,
    seed: Option<Seed>,
}

impl Facts {
    /// Records the device's CONNECT: credentials, clientId, and the receiver id
    /// (the MQTT username is `<rid>`; we fall back to stripping the clientId).
    fn apply_device_connect(&mut self, c: &mqtt_wire::Connect) {
        self.client_id.get_or_insert_with(|| c.client_id.clone());
        if let Some(u) = &c.username {
            self.mqtt_user.get_or_insert_with(|| u.clone());
            self.receiver_id.get_or_insert_with(|| u.clone());
        }
        if let Some(p) = &c.password {
            self.mqtt_pass.get_or_insert_with(|| p.clone());
        }
        self.receiver_id.get_or_insert_with(|| {
            c.client_id
                .strip_prefix(CLIENT_ID_PREFIX)
                .unwrap_or(&c.client_id)
                .to_string()
        });
    }

    /// Records a publish the device sent: firmware from `/log`, and (for `observe`,
    /// which has no cloud `/state`) the sensor id + a computed seed from `/read`.
    fn apply_device_publish(&mut self, topic: &str, payload: &[u8], now: &str) {
        if topic.ends_with("/log") {
            if let Some(fw) = firmware_from_log(payload) {
                self.firmware.get_or_insert(fw);
            }
        } else if topic.ends_with("/read") {
            if let Ok(reading) = Reading::parse(payload) {
                self.sensor_id.get_or_insert_with(|| reading.sensor.clone());
                self.sensor_name
                    .get_or_insert_with(|| reading.sensor.clone());
                // No history yet, so daysLeft starts at 0 and self-heals as `serve`
                // gathers readings; lstEmpty is unknown so we anchor it at "now".
                let cal = Calibration::default();
                self.seed.get_or_insert_with(|| Seed {
                    lvl: reading.level_cm,
                    pct: cal.pct(reading.level_cm),
                    bat: BatteryCurve::default().percent(reading.battery_mv),
                    days_left: 0,
                    lvl_to_full: cal.lvl_to_full(reading.level_cm),
                    lst_empty: now.to_string(),
                    from: "node-4".to_string(),
                });
            }
        }
    }

    /// Records a publish the cloud sent (only `learn` sees these): the firmware it
    /// echoes, the real retained `/state` (best seed + sensor identity), and the
    /// real `radarParams`.
    fn apply_cloud_publish(&mut self, topic: &str, payload: &[u8]) {
        if topic.starts_with("/version/") {
            // Only as a fallback — the firmware should come from the device's own
            // announcement (PRD AC: "not config", i.e. from the device itself).
            if let Ok(s) = std::str::from_utf8(payload) {
                let s = s.trim();
                if !s.is_empty() {
                    self.firmware.get_or_insert_with(|| s.to_string());
                }
            }
        } else if topic.ends_with("/state") {
            self.apply_cloud_state(payload);
        } else if topic.ends_with("/radarParams") {
            if let Ok(v) = serde_json::from_slice::<serde_json::Value>(payload) {
                if let Some(s) = v.get("sensor").and_then(|x| x.as_str()) {
                    if !s.is_empty() {
                        self.sensor_id.get_or_insert_with(|| s.to_string());
                    }
                }
                if let Some(n) = v.get("skip").and_then(|x| x.as_i64()) {
                    self.radar_skip.get_or_insert(n);
                }
                if let Some(n) = v.get("repeat").and_then(|x| x.as_i64()) {
                    self.radar_repeat.get_or_insert(n);
                }
            }
        }
    }

    /// Parses the cloud's retained `/state` into the seed + sensor identity.
    fn apply_cloud_state(&mut self, payload: &[u8]) {
        let Ok(v) = serde_json::from_slice::<serde_json::Value>(payload) else {
            return;
        };
        let Some(s) = v["sensors"].get(0) else {
            return;
        };
        if let Some(id) = s["id"].as_str() {
            self.sensor_id.get_or_insert_with(|| id.to_string());
        }
        if let Some(name) = s["name"].as_str() {
            self.sensor_name.get_or_insert_with(|| name.to_string());
        }
        let from = v["from"].as_str().unwrap_or("node-4").to_string();
        // The cloud's state is the authoritative seed: take it verbatim.
        self.seed.get_or_insert_with(|| Seed {
            lvl: s["lvl"].as_f64().unwrap_or(0.0),
            pct: s["pct"].as_i64().unwrap_or(0),
            bat: s["bat"].as_i64().unwrap_or(0),
            days_left: s["daysLeft"].as_i64().unwrap_or(0),
            lvl_to_full: s["lvlToFull"].as_i64().unwrap_or(0),
            lst_empty: s["lstEmpty"].as_str().unwrap_or("").to_string(),
            from,
        });
    }

    /// True once everything a usable `serve` config needs has been recovered.
    fn is_complete(&self) -> bool {
        self.receiver_id.is_some()
            && self.mqtt_user.is_some()
            && self.mqtt_pass.is_some()
            && self.firmware.is_some()
            && self.sensor_id.is_some()
            && self.seed.is_some()
    }

    /// One-line summary of what is still missing, for progress logging.
    fn missing(&self) -> Vec<&'static str> {
        let mut m = Vec::new();
        if self.receiver_id.is_none() {
            m.push("receiver_id");
        }
        if self.mqtt_user.is_none() {
            m.push("mqtt_user");
        }
        if self.mqtt_pass.is_none() {
            m.push("mqtt_pass");
        }
        if self.firmware.is_none() {
            m.push("firmware");
        }
        if self.sensor_id.is_none() {
            m.push("sensor_id");
        }
        if self.seed.is_none() {
            m.push("state");
        }
        m
    }
}

/// Best-effort firmware extraction from a `/log` payload. The exact format isn't
/// pinned down (the device reports a version plus its MAC), so we try JSON first,
/// then pick the first whitespace-delimited token that looks like a version
/// (`1.7.1.9_sh_en`) while skipping the MAC (which carries `:`).
fn firmware_from_log(payload: &[u8]) -> Option<String> {
    if let Ok(v) = serde_json::from_slice::<serde_json::Value>(payload) {
        for key in ["v", "version", "fw", "ver"] {
            if let Some(s) = v.get(key).and_then(|x| x.as_str()) {
                return Some(s.to_string());
            }
        }
    }
    let text = std::str::from_utf8(payload).ok()?;
    text.split_whitespace()
        .find(|tok| tok.contains('.') && !tok.contains(':') && tok.len() < 64)
        .map(|s| s.to_string())
}

/// Renders the generated `config.toml`. Mirrors `config.example.toml` so a user
/// can read and tweak it; the calibration is the documented default (40/178),
/// which reproduces the captured vendor samples and is the only piece neither mode
/// can recover from the wire (it lives server-side).
fn render_config(facts: &Facts, opts: &OnboardOptions) -> Result<String> {
    let missing = facts.missing();
    if !missing.is_empty() {
        anyhow::bail!("cannot write config; still missing: {}", missing.join(", "));
    }
    let rid = facts.receiver_id.as_ref().unwrap();
    let sid = facts.sensor_id.as_ref().unwrap();
    let cal = Calibration::default();
    let seed = facts.seed.as_ref().unwrap();
    let name = facts.sensor_name.clone().unwrap_or_else(|| sid.clone());

    Ok(format!(
        "# Generated by aquilo-server onboarding — recovered from your live device.\n\
         # All topics derive from receiver_id; the firmware came from the device's own\n\
         # /log announcement. Calibration is the documented default (40/178); adjust\n\
         # full_dist/empty_dist if pct/lvlToFull don't match your tank.\n\
         \n\
         receiver_id = \"{rid}\"\n\
         sensor_id = \"{sid}\"\n\
         mqtt_user = \"{user}\"\n\
         mqtt_pass = \"{pass}\"\n\
         firmware_version = \"{fw}\"\n\
         sensor_name = \"{name}\"\n\
         \n\
         radar_skip = {skip}\n\
         radar_repeat = {repeat}\n\
         \n\
         bind_addr = \"0.0.0.0\"\n\
         listen_port = 1883\n\
         ping_interval_secs = 1200\n\
         \n\
         data_dir = \"{data_dir}\"\n\
         history_max_len = 500\n\
         pump_out_drop_pct = 25\n\
         \n\
         [calibration]\n\
         full_dist = {full:?}\n\
         empty_dist = {empty:?}\n\
         \n\
         [state]\n\
         lvl = {lvl:?}\n\
         pct = {pct}\n\
         bat = {bat}\n\
         days_left = {days_left}\n\
         lvl_to_full = {lvl_to_full}\n\
         lst_empty = \"{lst_empty}\"\n\
         from = \"{from}\"\n",
        rid = rid,
        sid = sid,
        user = facts.mqtt_user.as_ref().unwrap(),
        pass = facts.mqtt_pass.as_ref().unwrap(),
        fw = facts.firmware.as_ref().unwrap(),
        name = name,
        skip = facts.radar_skip.unwrap_or(9),
        repeat = facts.radar_repeat.unwrap_or(9),
        data_dir = opts.data_dir,
        full = cal.full_dist,
        empty = cal.empty_dist,
        lvl = seed.lvl,
        pct = seed.pct,
        bat = seed.bat,
        days_left = seed.days_left,
        lvl_to_full = seed.lvl_to_full,
        lst_empty = seed.lst_empty,
        from = seed.from,
    ))
}

/// Writes the generated config and the initial state file. The state file carries
/// the seed as the last computed `/state` so `serve` re-seeds a valid retained
/// state immediately on its first start, before any live reading.
fn write_outputs(facts: &Facts, opts: &OnboardOptions, now: &str) -> Result<()> {
    let config = render_config(facts, opts)?;
    if let Some(parent) = std::path::Path::new(&opts.out_config).parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent).ok();
        }
    }
    std::fs::write(&opts.out_config, config)
        .with_context(|| format!("writing config to {}", opts.out_config))?;

    let seed = facts.seed.as_ref().unwrap();
    let sid = facts.sensor_id.clone().unwrap();
    let last_state = SensorState {
        id: sid.clone(),
        name: facts.sensor_name.clone().unwrap_or(sid),
        lvl: seed.lvl,
        pct: seed.pct,
        bat: seed.bat,
        lst_read: now.to_string(),
        lst_empty: seed.lst_empty.clone(),
        days_left: seed.days_left,
        lvl_to_full: seed.lvl_to_full,
        from: seed.from.clone(),
    };
    let store = JsonFileStore::in_dir(&opts.data_dir);
    store
        .save(&PersistedState {
            history: Vec::new(),
            lst_empty: seed.lst_empty.clone(),
            last_state: Some(last_state),
            calibration: Calibration::default(),
        })
        .context("writing initial state file")?;

    info!(
        config = %opts.out_config,
        data_dir = %opts.data_dir,
        receiver_id = %facts.receiver_id.as_ref().unwrap(),
        sensor_id = %facts.sensor_id.as_ref().unwrap(),
        firmware = %facts.firmware.as_ref().unwrap(),
        "onboarding complete — wrote config + initial state; run `serve` next"
    );
    Ok(())
}

/// Checks completeness and, the first time everything is present, writes the
/// outputs. The `AtomicBool` guards against writing twice across the two proxy
/// directions / repeated readings.
fn maybe_finalize(facts: &Mutex<Facts>, opts: &OnboardOptions, written: &AtomicBool) {
    let snapshot = {
        let f = facts.lock().unwrap();
        if !f.is_complete() {
            return;
        }
        f.clone()
    };
    if written.swap(true, Ordering::SeqCst) {
        return;
    }
    if let Err(e) = write_outputs(&snapshot, opts, &clock::now_rfc3339()) {
        warn!(error = %e, "failed to write onboarding output");
        written.store(false, Ordering::SeqCst); // let a later packet retry
    }
}

// --- learn: transparent proxy to the real cloud ---

/// Runs `learn`: accept the device, proxy it to the real cloud, and tee both
/// directions to recover the facts. Connections are handled one at a time (the
/// device reconnects serially); the shared [`Facts`] accumulate across them.
pub async fn run_learn(opts: LearnOptions) -> Result<()> {
    let listener = TcpListener::bind((opts.onboard.bind_addr.as_str(), opts.onboard.listen_port))
        .await
        .with_context(|| {
            format!(
                "binding learn listener {}:{}",
                opts.onboard.bind_addr, opts.onboard.listen_port
            )
        })?;
    info!(
        listen = %format!("{}:{}", opts.onboard.bind_addr, opts.onboard.listen_port),
        upstream = %format!("{}:{}", opts.upstream_host, opts.upstream_port),
        "learn mode: proxying the device to the real cloud and recording the handshake"
    );

    let facts = Arc::new(Mutex::new(Facts::default()));
    let written = Arc::new(AtomicBool::new(false));

    loop {
        let (client, peer) = listener.accept().await?;
        info!(%peer, "device connected; opening upstream to cloud");
        if let Err(e) = proxy_connection(client, &opts, &facts, &written).await {
            warn!(error = %e, "proxy connection ended");
        }
    }
}

async fn proxy_connection(
    client: TcpStream,
    opts: &LearnOptions,
    facts: &Arc<Mutex<Facts>>,
    written: &Arc<AtomicBool>,
) -> Result<()> {
    let upstream = TcpStream::connect((opts.upstream_host.as_str(), opts.upstream_port))
        .await
        .with_context(|| {
            format!(
                "connecting upstream {}:{}",
                opts.upstream_host, opts.upstream_port
            )
        })?;
    client.set_nodelay(true).ok();
    upstream.set_nodelay(true).ok();

    let (mut cr, mut cw) = client.into_split();
    let (mut ur, mut uw) = upstream.into_split();
    let onboard = opts.onboard.clone();

    // device -> cloud: forward verbatim, tee device packets into the facts.
    let c2s = {
        let facts = facts.clone();
        let written = written.clone();
        let onboard = onboard.clone();
        async move {
            let mut dec = Decoder::new();
            let mut buf = [0u8; 4096];
            loop {
                let n = cr.read(&mut buf).await?;
                if n == 0 {
                    break;
                }
                uw.write_all(&buf[..n]).await?;
                for pkt in dec.push(&buf[..n]) {
                    let now = clock::now_rfc3339();
                    let mut f = facts.lock().unwrap();
                    match pkt {
                        Packet::Connect(c) => {
                            info!(client_id = %c.client_id, "captured device CONNECT");
                            f.apply_device_connect(&c);
                        }
                        Packet::Publish(p) => f.apply_device_publish(&p.topic, &p.payload, &now),
                        _ => {}
                    }
                    drop(f);
                    maybe_finalize(&facts, &onboard, &written);
                }
            }
            Ok::<(), std::io::Error>(())
        }
    };

    // cloud -> device: forward verbatim, tee cloud publishes into the facts.
    let s2c = {
        let facts = facts.clone();
        let written = written.clone();
        async move {
            let mut dec = Decoder::new();
            let mut buf = [0u8; 4096];
            loop {
                let n = ur.read(&mut buf).await?;
                if n == 0 {
                    break;
                }
                cw.write_all(&buf[..n]).await?;
                for pkt in dec.push(&buf[..n]) {
                    if let Packet::Publish(p) = pkt {
                        info!(topic = %p.topic, retain = p.retain, "captured cloud PUBLISH");
                        facts
                            .lock()
                            .unwrap()
                            .apply_cloud_publish(&p.topic, &p.payload);
                    }
                }
                maybe_finalize(&facts, &onboard, &written);
            }
            Ok::<(), std::io::Error>(())
        }
    };

    // Either half closing tears down the pair.
    tokio::select! {
        r = c2s => r?,
        r = s2c => r?,
    }
    Ok(())
}

// --- observe: cloud-free stand-in broker ---

/// Runs `observe`: act as a minimal broker, complete the handshake, seed the
/// retained messages from documented defaults to keep the device alive, and
/// recover the identity from what the device announces — no cloud reachable.
pub async fn run_observe(opts: OnboardOptions) -> Result<()> {
    let listener = TcpListener::bind((opts.bind_addr.as_str(), opts.listen_port))
        .await
        .with_context(|| {
            format!(
                "binding observe listener {}:{}",
                opts.bind_addr, opts.listen_port
            )
        })?;
    info!(
        listen = %format!("{}:{}", opts.bind_addr, opts.listen_port),
        "observe mode: waiting for the device to connect (no cloud needed)"
    );

    let written = Arc::new(AtomicBool::new(false));
    loop {
        let (sock, peer) = listener.accept().await?;
        info!(%peer, "device connected");
        if let Err(e) = observe_connection(sock, &opts, &written).await {
            warn!(error = %e, "observe connection ended");
        }
    }
}

/// Per-connection observe state machine. Handles one device session: CONNACK the
/// CONNECT, SUBACK subscriptions, answer PINGREQ/QoS1, and once we know the
/// firmware + the device is subscribed, seed the retained messages so it reports
/// a reading — from which we recover the sensor id and finalize.
async fn observe_connection(
    mut sock: TcpStream,
    opts: &OnboardOptions,
    written: &Arc<AtomicBool>,
) -> Result<()> {
    sock.set_nodelay(true).ok();
    let facts = Mutex::new(Facts::default());
    let mut dec = Decoder::new();
    let mut subscribed = false;
    let mut seeded = false;
    let mut buf = [0u8; 4096];

    loop {
        let n = sock.read(&mut buf).await?;
        if n == 0 {
            break; // device closed the connection
        }
        for pkt in dec.push(&buf[..n]) {
            match pkt {
                Packet::Connect(c) => {
                    info!(client_id = %c.client_id, user = ?c.username, "device CONNECT");
                    facts.lock().unwrap().apply_device_connect(&c);
                    sock.write_all(&mqtt_wire::connack()).await?;
                }
                Packet::Subscribe(s) => {
                    sock.write_all(&mqtt_wire::suback(s.packet_id, s.topics.len()))
                        .await?;
                    subscribed = true;
                }
                Packet::Publish(p) => {
                    if let Some(id) = p.packet_id {
                        sock.write_all(&mqtt_wire::puback(id)).await?;
                    }
                    let now = clock::now_rfc3339();
                    let mut f = facts.lock().unwrap();
                    if p.topic.ends_with("/read") {
                        info!(topic = %p.topic, "device reading received");
                    } else {
                        info!(topic = %p.topic, payload = %String::from_utf8_lossy(&p.payload), "device publish");
                    }
                    f.apply_device_publish(&p.topic, &p.payload, &now);
                    drop(f);
                    maybe_finalize(&facts, opts, written);
                }
                Packet::PingReq => sock.write_all(&mqtt_wire::pingresp()).await?,
                Packet::Disconnect => return Ok(()),
                Packet::Other { .. } => {}
            }
        }

        // Seed the retained connect-time messages once we can: the device must be
        // subscribed (so it receives them) and have announced its firmware (so we
        // echo the right version and don't trigger an OTA). This is what coaxes it
        // out of the reconnect cycle and into publishing a reading.
        if subscribed && !seeded {
            let (rid, fw) = {
                let f = facts.lock().unwrap();
                (f.receiver_id.clone(), f.firmware.clone())
            };
            if let (Some(rid), Some(fw)) = (rid, fw) {
                seed_retained(&mut sock, &rid, &fw, &facts).await?;
                seeded = true;
                info!("seeded retained version/state/radarParams from defaults");
            }
        }
    }
    Ok(())
}

/// Publishes the three retained connect-time messages from documented defaults,
/// mirroring what `serve` seeds — enough to keep the device connected while we
/// wait for a reading. The sensor id may still be unknown here, so `/state` and
/// `radarParams` use a placeholder; the real value is captured from `/read` and
/// written into the config.
async fn seed_retained(
    sock: &mut TcpStream,
    rid: &str,
    firmware: &str,
    facts: &Mutex<Facts>,
) -> Result<()> {
    let topics = crate::topics::Topics::new(rid);
    let sensor = facts
        .lock()
        .unwrap()
        .sensor_id
        .clone()
        .unwrap_or_else(|| rid.to_string());

    sock.write_all(&mqtt_wire::publish(
        &topics.version,
        firmware.as_bytes(),
        true,
    ))
    .await?;

    let radar = json!({ "sensor": sensor, "skip": 9, "repeat": 9 });
    sock.write_all(&mqtt_wire::publish(
        &topics.radar_params,
        &serde_json::to_vec(&radar)?,
        true,
    ))
    .await?;

    let state = default_seed_state(&sensor).to_json();
    sock.write_all(&mqtt_wire::publish(
        &topics.state,
        &serde_json::to_vec(&state)?,
        true,
    ))
    .await?;
    Ok(())
}

/// A neutral placeholder `/state` (the documented sample) the device can serve
/// until `serve` republishes a computed one. `serve` overwrites this on its first
/// reading; it exists only so the device has a valid state during onboarding.
fn default_seed_state(sensor: &str) -> SensorState {
    SensorState {
        id: sensor.to_string(),
        name: sensor.to_string(),
        lvl: 150.2,
        pct: 20,
        bat: 83,
        lst_read: clock::now_rfc3339(),
        lst_empty: String::new(),
        days_left: 0,
        lvl_to_full: 110,
        from: "node-4".to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;

    fn opts() -> OnboardOptions {
        OnboardOptions {
            bind_addr: "127.0.0.1".into(),
            listen_port: 1883,
            out_config: "/tmp/unused.toml".into(),
            data_dir: "data".into(),
        }
    }

    #[test]
    fn learn_facts_from_connect_and_cloud_state_yield_a_loadable_config() {
        let mut f = Facts::default();
        f.apply_device_connect(&mqtt_wire::Connect {
            client_id: "CieczSensorae83fc".into(),
            username: Some("ae83fc".into()),
            password: Some("48007129".into()),
        });
        f.apply_device_publish(
            "/users/ae83fc/sensors/ae83fc/log",
            b"1.7.1.9_sh_en D8:BC:38:AE:83:FC",
            "2026-06-11T12:00:00+02:00",
        );
        f.apply_cloud_publish(
            "/users/ae83fc/receivers/ae83fc/radarParams",
            br#"{"sensor":"ae5058","skip":9,"repeat":9,"from":"ovh4-docker"}"#,
        );
        f.apply_cloud_publish(
            "/users/ae83fc/state",
            br#"{"sensors":[{"id":"ae5058","lvl":150.2,"pct":20,"bat":83,"lstRead":"x","lstEmpty":"2026-05-30T00:17:19+02:00","daysLeft":51,"name":"ae5058","lvlToFull":110}],"from":"node-4"}"#,
        );
        assert!(f.is_complete(), "missing: {:?}", f.missing());

        let rendered = render_config(&f, &opts()).unwrap();
        // The whole point of AC2: serve consumes it with no hand-editing.
        let cfg: Config = toml::from_str(&rendered).expect("generated config must parse");
        assert_eq!(cfg.receiver_id, "ae83fc");
        assert_eq!(cfg.sensor_id, "ae5058");
        assert_eq!(cfg.mqtt_user, "ae83fc");
        assert_eq!(cfg.mqtt_pass, "48007129");
        assert_eq!(cfg.firmware_version, "1.7.1.9_sh_en");
        assert_eq!(cfg.state.lvl, 150.2);
        assert_eq!(cfg.state.pct, 20);
        assert_eq!(cfg.state.days_left, 51);
        assert_eq!(cfg.state.lst_empty, "2026-05-30T00:17:19+02:00");
        assert_eq!(cfg.calibration.full_dist, 40.0);
    }

    #[test]
    fn observe_facts_reconstruct_identity_from_device_only() {
        let mut f = Facts::default();
        // No cloud: identity comes purely from the device CONNECT + /log + /read.
        f.apply_device_connect(&mqtt_wire::Connect {
            client_id: "CieczSensorae83fc".into(),
            username: Some("ae83fc".into()),
            password: Some("48007129".into()),
        });
        assert_eq!(f.receiver_id.as_deref(), Some("ae83fc"));
        f.apply_device_publish(
            "/users/ae83fc/sensors/ae83fc/log",
            br#"{"v":"1.7.1.9_sh_en","mac":"D8:BC:38:AE:83:FC"}"#,
            "2026-06-11T12:00:00+02:00",
        );
        assert!(!f.is_complete(), "no reading yet → no sensor id/seed");

        f.apply_device_publish(
            "/users/ae83fc/sensors/ae83fc/read",
            br#"{"sensor":"ae5058","read1":15280,"battery":"3770","raw":"_e23_ae5058_1528_3770_59_0_73045_72976_1"}"#,
            "2026-06-11T12:01:00+02:00",
        );
        assert!(f.is_complete(), "missing: {:?}", f.missing());
        assert_eq!(f.sensor_id.as_deref(), Some("ae5058"));

        let cfg: Config = toml::from_str(&render_config(&f, &opts()).unwrap()).unwrap();
        assert_eq!(cfg.firmware_version, "1.7.1.9_sh_en");
        assert_eq!(cfg.sensor_id, "ae5058");
        // Reading 152.8 + default calibration → pct 18, lvlToFull 113.
        assert_eq!(cfg.state.lvl, 152.8);
        assert_eq!(cfg.state.pct, 18);
        assert_eq!(cfg.state.lvl_to_full, 113);
        // lstEmpty unknown offline → anchored at the reading time (self-heals).
        assert_eq!(cfg.state.lst_empty, "2026-06-11T12:01:00+02:00");
    }

    #[test]
    fn firmware_comes_from_the_device_not_the_cloud_version() {
        // Device announces e23 in /log; cloud later echoes a different /version.
        // AC: firmware is taken from the device's own announcement.
        let mut f = Facts::default();
        f.apply_device_publish("/x/log", b"7.7.7_dev AA:BB", "t");
        f.apply_cloud_publish("/version/czujnik_szamba/ae83fc", b"9.9.9_cloud");
        assert_eq!(f.firmware.as_deref(), Some("7.7.7_dev"));
    }

    #[test]
    fn write_outputs_produces_a_state_file_serve_can_load() {
        let dir = tempfile::tempdir().unwrap();
        let cfg_path = dir.path().join("config.toml");
        let o = OnboardOptions {
            bind_addr: "127.0.0.1".into(),
            listen_port: 1883,
            out_config: cfg_path.to_str().unwrap().into(),
            data_dir: dir.path().to_str().unwrap().into(),
        };
        let mut f = Facts::default();
        f.apply_device_connect(&mqtt_wire::Connect {
            client_id: "CieczSensorae83fc".into(),
            username: Some("ae83fc".into()),
            password: Some("48007129".into()),
        });
        f.apply_device_publish("/x/log", b"1.7.1.9_sh_en", "t");
        f.apply_device_publish(
            "/x/read",
            br#"{"sensor":"ae5058","read1":15280,"battery":"3770"}"#,
            "2026-06-11T12:01:00+02:00",
        );

        write_outputs(&f, &o, "2026-06-11T12:02:00+02:00").unwrap();

        // Config loads via the real Config::load path (no hand-editing).
        let loaded = Config::load(o.out_config.as_str()).unwrap();
        assert_eq!(loaded.sensor_id, "ae5058");

        // The state file is what serve re-seeds from on startup.
        let store = JsonFileStore::in_dir(&o.data_dir);
        let persisted = store.load().unwrap().unwrap();
        let last = persisted.last_state.unwrap();
        assert_eq!(last.id, "ae5058");
        assert_eq!(last.lvl, 152.8);
        assert_eq!(last.lst_read, "2026-06-11T12:02:00+02:00");
    }
}
