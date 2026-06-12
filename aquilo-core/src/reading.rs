//! Raw-reading parser: turns a device `/read` payload into a structured [`Reading`].
//!
//! The payload carries the measurement twice — as explicit JSON fields (`read1`,
//! `battery`, `readNo`, …) and packed into an underscore-delimited `raw` string.
//! We decode both: the JSON fields are canonical (the formulas are defined in terms
//! of `read1`), and the `raw` string is parsed into a [`RawFrame`] for the counters
//! and as a cross-check.

use anyhow::{anyhow, Context, Result};
use serde::Deserialize;

/// A single measurement from the radio sensor node, ready for state computation.
#[derive(Clone, Debug, PartialEq)]
pub struct Reading {
    /// Sensor-node id (e.g. `ae5058`).
    pub sensor: String,
    /// Firmware/protocol tag the reading was produced with (the `v` field, e.g. `e23`).
    pub version: Option<String>,
    /// Radar distance to the liquid surface, in cm (`read1 / 100`). Smaller = fuller.
    pub level_cm: f64,
    /// Battery voltage in millivolts.
    pub battery_mv: i64,
    /// Monotonic reading counter reported by the device.
    pub read_no: Option<i64>,
    /// Counter of the previously transmitted reading.
    pub last_sent: Option<i64>,
    /// Radar amplitude of the strongest echo.
    pub amp1: Option<i64>,
    /// Reported temperature (0 when the sensor has no probe).
    pub temp: Option<i64>,
    /// The decoded `raw` string, when present.
    pub raw: Option<RawFrame>,
}

/// The underscore-delimited `raw` telemetry string, decoded positionally.
///
/// Example: `_e23_ae5058_1502_3770_59_0_73044_72976_1_2_1_0_0_0`.
#[derive(Clone, Debug, PartialEq)]
pub struct RawFrame {
    /// Protocol tag (`e23`).
    pub version: String,
    /// Sensor-node id.
    pub sensor: String,
    /// Level in millimetres (`1502` → 150.2 cm).
    pub level_mm: i64,
    /// Battery voltage in millivolts.
    pub battery_mv: i64,
    /// Radar amplitude.
    pub amp: i64,
    /// Temperature.
    pub temp: i64,
    /// Reading counter.
    pub read_no: i64,
    /// Previously transmitted reading counter.
    pub last_sent: i64,
    /// Trailing fields whose meaning is not yet reverse-engineered.
    pub rest: Vec<i64>,
}

/// The JSON envelope the device publishes on `/read`. Only the fields we consume
/// are named; the rest (`salt`, `crc`, `rssi`, …) are ignored.
#[derive(Deserialize)]
struct ReadPayload {
    sensor: Option<String>,
    #[serde(rename = "v")]
    version: Option<String>,
    read1: Option<i64>,
    battery: Option<String>,
    #[serde(rename = "readNo")]
    read_no: Option<i64>,
    #[serde(rename = "lastSent")]
    last_sent: Option<i64>,
    amp1: Option<i64>,
    temp: Option<i64>,
    raw: Option<String>,
}

impl RawFrame {
    /// Parses the underscore-delimited telemetry string. The string is expected to
    /// start with an empty leading segment (a leading underscore) and carry at
    /// least the eight known positional fields.
    pub fn parse(raw: &str) -> Result<Self> {
        // The leading underscore yields an empty first segment; drop empties so a
        // leading or trailing `_` doesn't shift the field positions.
        let parts: Vec<&str> = raw.split('_').filter(|s| !s.is_empty()).collect();
        if parts.len() < 8 {
            return Err(anyhow!(
                "raw string has {} fields, expected at least 8: {raw:?}",
                parts.len()
            ));
        }

        let num = |idx: usize| -> Result<i64> {
            parts[idx]
                .parse::<i64>()
                .with_context(|| format!("raw field {idx} ({:?}) is not an integer", parts[idx]))
        };

        Ok(RawFrame {
            version: parts[0].to_string(),
            sensor: parts[1].to_string(),
            level_mm: num(2)?,
            battery_mv: num(3)?,
            amp: num(4)?,
            temp: num(5)?,
            read_no: num(6)?,
            last_sent: num(7)?,
            rest: parts[8..]
                .iter()
                .map(|s| s.parse::<i64>())
                .collect::<std::result::Result<_, _>>()
                .context("trailing raw field is not an integer")?,
        })
    }
}

impl Reading {
    /// Parses a captured `/read` JSON payload into a [`Reading`].
    pub fn parse(payload: &[u8]) -> Result<Self> {
        let p: ReadPayload = serde_json::from_slice(payload).context("invalid /read JSON")?;
        let raw = p.raw.as_deref().map(RawFrame::parse).transpose()?;

        // Level comes from `read1` per the vendor formula; fall back to the raw
        // millimetre field when the JSON omits it.
        let level_cm = match (p.read1, &raw) {
            (Some(read1), _) => read1 as f64 / 100.0,
            (None, Some(frame)) => frame.level_mm as f64 / 10.0,
            (None, None) => return Err(anyhow!("/read payload has neither read1 nor raw level")),
        };

        let battery_mv = match (p.battery.as_deref(), &raw) {
            (Some(b), _) => b
                .parse::<i64>()
                .with_context(|| format!("battery {b:?} is not an integer (mV)"))?,
            (None, Some(frame)) => frame.battery_mv,
            (None, None) => return Err(anyhow!("/read payload has no battery voltage")),
        };

        let sensor = p
            .sensor
            .or_else(|| raw.as_ref().map(|f| f.sensor.clone()))
            .ok_or_else(|| anyhow!("/read payload has no sensor id"))?;

        Ok(Reading {
            sensor,
            version: p
                .version
                .or_else(|| raw.as_ref().map(|f| f.version.clone())),
            level_cm,
            battery_mv,
            read_no: p.read_no.or_else(|| raw.as_ref().map(|f| f.read_no)),
            last_sent: p.last_sent.or_else(|| raw.as_ref().map(|f| f.last_sent)),
            amp1: p.amp1.or_else(|| raw.as_ref().map(|f| f.amp)),
            temp: p.temp.or_else(|| raw.as_ref().map(|f| f.temp)),
            raw,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const CAPTURED: &[u8] = br#"{"salt":"abc","raw":"_e23_ae5058_1502_3770_59_0_73044_72976_1_2_1_0_0_0","v":"e23","sensor":"ae5058","read1":15020,"battery":"3770","crc":true,"amp1":59,"temp":0,"readNo":73044,"lastSent":72976,"rev":1,"r":2,"rssi":-75,"wifi":-80,"snr":5}"#;

    #[test]
    fn parses_captured_read_into_structured_reading() {
        let r = Reading::parse(CAPTURED).unwrap();
        assert_eq!(r.sensor, "ae5058");
        assert_eq!(r.version.as_deref(), Some("e23"));
        assert_eq!(r.level_cm, 150.2);
        assert_eq!(r.battery_mv, 3770);
        assert_eq!(r.read_no, Some(73044));
        assert_eq!(r.last_sent, Some(72976));
        assert_eq!(r.amp1, Some(59));
        assert_eq!(r.temp, Some(0));
    }

    #[test]
    fn decodes_the_raw_string_positionally() {
        let frame = RawFrame::parse("_e23_ae5058_1502_3770_59_0_73044_72976_1_2_1_0_0_0").unwrap();
        assert_eq!(
            frame,
            RawFrame {
                version: "e23".into(),
                sensor: "ae5058".into(),
                level_mm: 1502,
                battery_mv: 3770,
                amp: 59,
                temp: 0,
                read_no: 73044,
                last_sent: 72976,
                rest: vec![1, 2, 1, 0, 0, 0],
            }
        );
    }

    #[test]
    fn raw_level_and_json_read1_agree() {
        let r = Reading::parse(CAPTURED).unwrap();
        let frame = r.raw.as_ref().unwrap();
        assert_eq!(r.level_cm, frame.level_mm as f64 / 10.0);
    }

    #[test]
    fn falls_back_to_raw_when_json_fields_missing() {
        let payload = br#"{"raw":"_e23_ae5058_1528_3770_59_0_73045_72976_1"}"#;
        let r = Reading::parse(payload).unwrap();
        assert_eq!(r.sensor, "ae5058");
        assert_eq!(r.level_cm, 152.8);
        assert_eq!(r.battery_mv, 3770);
        assert_eq!(r.read_no, Some(73045));
    }

    #[test]
    fn rejects_payload_without_level() {
        let payload = br#"{"sensor":"ae5058","battery":"3770"}"#;
        assert!(Reading::parse(payload).is_err());
    }

    #[test]
    fn rejects_short_raw_string() {
        assert!(RawFrame::parse("_e23_ae5058_1502").is_err());
    }
}
