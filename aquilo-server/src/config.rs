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

    /// Seed values for the retained `/state` published before the first reading.
    pub state: StateSeed,
}

#[derive(Clone, Debug, Deserialize)]
pub struct StateSeed {
    /// Raw radar level (cm) of the last known reading.
    pub lvl: f64,
    pub pct: i64,
    pub bat: i64,
    pub days_left: i64,
    pub lvl_to_full: i64,
    /// Last pump-out timestamp (RFC3339).
    pub lst_empty: String,
    #[serde(default = "defaults::from")]
    pub from: String,
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
}
