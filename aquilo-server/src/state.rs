//! The `/state` payload the server republishes and the device serves over HTTP.

use serde_json::{json, Value};

use crate::config::Config;

#[derive(Clone, Debug)]
pub struct SensorState {
    pub id: String,
    pub name: String,
    pub lvl: f64,
    pub pct: i64,
    pub bat: i64,
    pub lst_read: String,
    pub lst_empty: String,
    pub days_left: i64,
    pub lvl_to_full: i64,
    pub from: String,
}

impl SensorState {
    /// Builds the initial state from the configured seed values.
    pub fn seed(cfg: &Config, lst_read: String) -> Self {
        let s = &cfg.state;
        Self {
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

    /// The vendor `/state` JSON shape, byte-for-byte field names.
    pub fn to_json(&self) -> Value {
        json!({
            "sensors": [{
                "id": self.id,
                "lvl": self.lvl,
                "pct": self.pct,
                "bat": self.bat,
                "lstRead": self.lst_read,
                "lstEmpty": self.lst_empty,
                "daysLeft": self.days_left,
                "name": self.name,
                "lvlToFull": self.lvl_to_full,
            }],
            "from": self.from,
        })
    }
}

/// Current local time as RFC3339 with offset (e.g. `2026-06-10T20:44:35+02:00`),
/// matching the `lstRead` the vendor cloud stamps.
pub fn now_rfc3339() -> String {
    chrono::Local::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, false)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn produces_vendor_state_shape() {
        let state = SensorState {
            id: "ae5058".into(),
            name: "ae5058".into(),
            lvl: 152.8,
            pct: 18,
            bat: 83,
            lst_read: "2026-06-10T20:44:35+02:00".into(),
            lst_empty: "2026-05-30T00:17:19+02:00".into(),
            days_left: 51,
            lvl_to_full: 110,
            from: "node-4".into(),
        };
        let json = state.to_json();
        let sensor = &json["sensors"][0];
        assert_eq!(sensor["id"], "ae5058");
        assert_eq!(sensor["lvl"], 152.8);
        assert_eq!(sensor["lstRead"], "2026-06-10T20:44:35+02:00");
        assert_eq!(sensor["lstEmpty"], "2026-05-30T00:17:19+02:00");
        assert_eq!(sensor["daysLeft"], 51);
        assert_eq!(sensor["lvlToFull"], 110);
        assert_eq!(json["from"], "node-4");
    }
}
