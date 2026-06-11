//! Minimal raw-reading parser for phase 1: just enough to pull the level out of
//! a `/read` payload. The full `aquilo-core` parser (raw string, counters,
//! battery curve) arrives in phase 2.

use serde::Deserialize;

#[derive(Debug, Deserialize)]
pub struct RawRead {
    /// Radar distance in 1/100 cm (e.g. 15020 → 150.2 cm).
    pub read1: Option<i64>,
    pub sensor: Option<String>,
    pub battery: Option<String>,
    #[serde(default)]
    pub raw: Option<String>,
}

impl RawRead {
    pub fn parse(payload: &[u8]) -> anyhow::Result<Self> {
        Ok(serde_json::from_slice(payload)?)
    }

    /// Level in cm, taken straight from the raw reading (no calibration yet).
    pub fn level(&self) -> Option<f64> {
        self.read1.map(|r| r as f64 / 100.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_captured_read_and_derives_level() {
        let payload = br#"{"salt":"x","raw":"_e23_ae5058_1502_3770","sensor":"ae5058","read1":15020,"battery":"3770"}"#;
        let reading = RawRead::parse(payload).unwrap();
        assert_eq!(reading.read1, Some(15020));
        assert_eq!(reading.sensor.as_deref(), Some("ae5058"));
        assert_eq!(reading.level(), Some(150.2));
    }
}
