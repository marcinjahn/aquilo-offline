//! Device/tank configuration. Everything device-specific lives here rather than
//! in source, so the binary is generic and shareable (PRD: Configuration).

use aquilo_core::Calibration;
use serde::Deserialize;

#[derive(Clone, Debug, Deserialize)]
pub struct Config {
    /// Receiver/gateway id (`<rid>`); all topics are templated from it.
    pub receiver_id: String,
    /// Radio sensor-node id reported inside `/state`.
    pub sensor_id: String,
    /// Fixed MQTT credentials the device's firmware connects with.
    pub mqtt_user: String,
    pub mqtt_pass: String,
    /// Firmware string echoed back on `/version/...` so no OTA is triggered.
    pub firmware_version: String,
    /// Display name for the sensor in `/state`.
    pub sensor_name: String,

    #[serde(default = "defaults::radar_skip")]
    pub radar_skip: i64,
    #[serde(default = "defaults::radar_repeat")]
    pub radar_repeat: i64,

    #[serde(default = "defaults::bind_addr")]
    pub bind_addr: String,
    #[serde(default = "defaults::listen_port")]
    pub listen_port: u16,
    #[serde(default = "defaults::ping_interval_secs")]
    pub ping_interval_secs: u64,

    /// Directory holding the persisted state file. In the HA add-on this is the
    /// `/data` volume, which survives restarts, host reboots and add-on updates.
    #[serde(default = "defaults::data_dir")]
    pub data_dir: String,
    /// Cap on retained history records; the oldest are dropped past this.
    #[serde(default = "defaults::history_max_len")]
    pub history_max_len: usize,
    /// Percentage-point fall in fullness between consecutive readings that counts
    /// as a pump-out and resets the `lstEmpty` baseline.
    #[serde(default = "defaults::pump_out_drop_pct")]
    pub pump_out_drop_pct: i64,

    /// Tank calibration driving `pct`/`lvlToFull` (PRD user story 12).
    #[serde(default)]
    pub calibration: Calibration,

    /// External broker to connect to as a client instead of running the embedded
    /// one. Set by the Home Assistant add-on, where the standard Mosquitto add-on
    /// owns the device's connection and retained-message persistence. When absent
    /// (the standalone Docker build) `serve` spawns its own in-process `rumqttd`.
    #[serde(default)]
    pub broker: Option<BrokerSettings>,

    /// Seed values for the retained `/state` published before the first reading.
    /// Defaulted so a fresh add-on need not configure a placeholder state; the
    /// first live reading overwrites it within a day.
    #[serde(default)]
    pub state: StateSeed,
}

/// How the `serve` client reaches the broker when one already exists (the HA
/// Mosquitto add-on). These are the credentials the *server* authenticates with;
/// the device authenticates separately with `mqtt_user`/`mqtt_pass`.
#[derive(Clone, Debug, Deserialize)]
pub struct BrokerSettings {
    pub host: String,
    #[serde(default = "defaults::listen_port")]
    pub port: u16,
    pub username: String,
    pub password: String,
}

#[derive(Clone, Debug, Deserialize)]
pub struct StateSeed {
    /// Raw radar level (cm) of the last known reading.
    #[serde(default = "defaults::seed_lvl")]
    pub lvl: f64,
    #[serde(default = "defaults::seed_pct")]
    pub pct: i64,
    #[serde(default = "defaults::seed_bat")]
    pub bat: i64,
    #[serde(default)]
    pub days_left: i64,
    #[serde(default = "defaults::seed_lvl_to_full")]
    pub lvl_to_full: i64,
    /// Last pump-out timestamp (RFC3339). Empty until known; it self-heals on the
    /// first detected pump-out.
    #[serde(default)]
    pub lst_empty: String,
    #[serde(default = "defaults::from")]
    pub from: String,
}

impl Default for StateSeed {
    fn default() -> Self {
        StateSeed {
            lvl: defaults::seed_lvl(),
            pct: defaults::seed_pct(),
            bat: defaults::seed_bat(),
            days_left: 0,
            lvl_to_full: defaults::seed_lvl_to_full(),
            lst_empty: String::new(),
            from: defaults::from(),
        }
    }
}

impl Config {
    pub fn load(path: &str) -> anyhow::Result<Config> {
        let text = std::fs::read_to_string(path)?;
        Ok(toml::from_str(&text)?)
    }
}

mod defaults {
    pub fn radar_skip() -> i64 {
        9
    }
    pub fn radar_repeat() -> i64 {
        9
    }
    pub fn bind_addr() -> String {
        "0.0.0.0".to_string()
    }
    pub fn listen_port() -> u16 {
        1883
    }
    pub fn ping_interval_secs() -> u64 {
        1200
    }
    pub fn data_dir() -> String {
        "/data".to_string()
    }
    pub fn history_max_len() -> usize {
        500
    }
    pub fn pump_out_drop_pct() -> i64 {
        25
    }
    pub fn from() -> String {
        "node-4".to_string()
    }
    // Neutral placeholder `/state` shown until the first live reading lands. Mirrors
    // the documented vendor sample so the HA RESTful sensor sees a plausible state.
    pub fn seed_lvl() -> f64 {
        150.2
    }
    pub fn seed_pct() -> i64 {
        20
    }
    pub fn seed_bat() -> i64 {
        83
    }
    pub fn seed_lvl_to_full() -> i64 {
        110
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The minimal device-identity config the HA add-on renders: no `[state]`
    /// table (it defaults to a placeholder) and an external `[broker]` so `serve`
    /// connects as a Mosquitto client instead of spawning the embedded broker.
    #[test]
    fn add_on_style_config_with_external_broker_loads() {
        let toml = r#"
            receiver_id = "ae83fc"
            sensor_id = "ae5058"
            mqtt_user = "ae83fc"
            mqtt_pass = "48007129"
            firmware_version = "1.7.1.9_sh_en"
            sensor_name = "Septic tank"

            [calibration]
            full_dist = 40.0
            empty_dist = 178.0

            [broker]
            host = "core-mosquitto"
            username = "addons"
            password = "secret"
        "#;
        let cfg: Config = toml::from_str(toml).unwrap();
        let broker = cfg.broker.expect("external broker configured");
        assert_eq!(broker.host, "core-mosquitto");
        assert_eq!(broker.port, 1883, "port defaults to 1883");
        assert_eq!(broker.username, "addons");
        // No [state] table → the documented placeholder seed is used.
        assert_eq!(cfg.state.lvl, 150.2);
        assert_eq!(cfg.state.lst_empty, "");
    }

    /// The standalone (embedded-broker) config omits `[broker]` entirely.
    #[test]
    fn config_without_broker_section_runs_embedded() {
        let toml = r#"
            receiver_id = "ae83fc"
            sensor_id = "ae5058"
            mqtt_user = "ae83fc"
            mqtt_pass = "48007129"
            firmware_version = "1.7.1.9_sh_en"
            sensor_name = "ae5058"
        "#;
        let cfg: Config = toml::from_str(toml).unwrap();
        assert!(cfg.broker.is_none());
    }
}
