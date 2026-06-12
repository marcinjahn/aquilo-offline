//! The vendor `/state` payload the server republishes and the device serves over
//! HTTP, plus the computation that derives it from a reading.

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::battery::BatteryCurve;
use crate::calibration::Calibration;
use crate::reading::Reading;

/// Fields that don't (yet) derive from a single reading: identity plus the
/// history-backed values. History-based computation of `lstEmpty`/`daysLeft`
/// arrives with persistence in a later phase; for now they are carried through
/// from the last known state.
#[derive(Clone, Debug)]
pub struct StaticFields {
    pub sensor_id: String,
    pub name: String,
    /// Last pump-out timestamp (RFC3339).
    pub lst_empty: String,
    pub days_left: i64,
    /// The `from` field the vendor stamps (the receiving node, e.g. `node-4`).
    pub from: String,
}

/// The computed sensor state, mirroring the vendor `/state` JSON one-to-one.
///
/// Derives serde so the last computed state can be persisted verbatim and
/// re-seeded on restart. The serialized form uses the Rust field names (our own
/// store format); the vendor `/state` shape is produced separately by
/// [`SensorState::to_json`].
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
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
    /// Computes the full state from a reading, calibration, battery curve and the
    /// carried-through static fields. `lst_read` is injected by the caller (the
    /// time the reading was received) so the computation is deterministic.
    pub fn compute(
        reading: &Reading,
        calibration: &Calibration,
        battery: &BatteryCurve,
        statics: &StaticFields,
        lst_read: String,
    ) -> Self {
        let lvl = reading.level_cm;
        SensorState {
            id: statics.sensor_id.clone(),
            name: statics.name.clone(),
            lvl,
            pct: calibration.pct(lvl),
            bat: battery.percent(reading.battery_mv),
            lst_read,
            lst_empty: statics.lst_empty.clone(),
            days_left: statics.days_left,
            lvl_to_full: calibration.lvl_to_full(lvl),
            from: statics.from.clone(),
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

#[cfg(test)]
mod tests {
    use super::*;

    fn statics() -> StaticFields {
        StaticFields {
            sensor_id: "ae5058".into(),
            name: "ae5058".into(),
            lst_empty: "2026-05-30T00:17:19+02:00".into(),
            days_left: 51,
            from: "node-4".into(),
        }
    }

    #[test]
    fn computes_state_from_a_captured_read() {
        let payload = br#"{"raw":"_e23_ae5058_1528_3770_59_0_73045_72976_1","sensor":"ae5058","read1":15280,"battery":"3770"}"#;
        let reading = Reading::parse(payload).unwrap();
        let state = SensorState::compute(
            &reading,
            &Calibration::default(),
            &BatteryCurve::default(),
            &statics(),
            "2026-06-10T20:44:35+02:00".into(),
        );

        let json = state.to_json();
        let sensor = &json["sensors"][0];
        assert_eq!(sensor["id"], "ae5058");
        assert_eq!(sensor["lvl"], 152.8);
        assert_eq!(sensor["pct"], 18);
        assert_eq!(sensor["bat"], 83);
        assert_eq!(sensor["lvlToFull"], 113);
        // Injected time and carried-through static fields appear verbatim.
        assert_eq!(sensor["lstRead"], "2026-06-10T20:44:35+02:00");
        assert_eq!(sensor["lstEmpty"], "2026-05-30T00:17:19+02:00");
        assert_eq!(sensor["daysLeft"], 51);
        assert_eq!(sensor["name"], "ae5058");
        assert_eq!(json["from"], "node-4");
    }

    #[test]
    fn changing_calibration_changes_derived_values() {
        let reading =
            Reading::parse(br#"{"sensor":"ae5058","read1":15020,"battery":"3770"}"#).unwrap();
        let base = SensorState::compute(
            &reading,
            &Calibration::default(),
            &BatteryCurve::default(),
            &statics(),
            "t".into(),
        );
        assert_eq!(base.pct, 20);
        assert_eq!(base.lvl_to_full, 110);

        let recal = SensorState::compute(
            &reading,
            &Calibration {
                full_dist: 30.0,
                empty_dist: 200.0,
            },
            &BatteryCurve::default(),
            &statics(),
            "t".into(),
        );
        assert_eq!(recal.pct, 29);
        assert_eq!(recal.lvl_to_full, 120);
    }
}
