//! Reading history and the history-dependent projections the vendor cloud
//! computed for us: pump-out detection (a sharp fullness drop resets the
//! `lstEmpty` baseline) and the `daysLeft` fill-rate estimate.
//!
//! These are pure functions over a slice of [`ReadingRecord`]; time enters only
//! as the RFC3339 timestamps already stored on the records, so the projections
//! are deterministic under test.

use serde::{Deserialize, Serialize};

/// One stored reading: when it was received (RFC3339 with offset), the radar
/// distance to the surface (cm; smaller = fuller), and the battery percentage at
/// that time. This is the rolling history the projections run over.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ReadingRecord {
    pub ts: String,
    pub lvl: f64,
    pub bat: i64,
}

/// Detects a pump-out from the fullness drop between the previous and the current
/// reading. Emptying the tank shows up as a sharp fall in `pct`; a drop of at
/// least `drop_threshold_pct` percentage points is treated as a pump-out, which
/// the caller uses to reset the `lstEmpty` baseline to the new reading's time.
pub fn is_pump_out(prev_pct: i64, new_pct: i64, drop_threshold_pct: i64) -> bool {
    prev_pct - new_pct >= drop_threshold_pct
}

/// Projects the number of days until the tank is full — the radar distance falls
/// to `full_dist` — from a least-squares fit of distance against time over the
/// history.
///
/// Returns `None` when there isn't enough signal to project: fewer than two
/// readings, all readings at the same instant, or a trend that isn't filling (a
/// flat or rising distance, where the vendor's "days left" has no meaning).
pub fn days_left(history: &[ReadingRecord], full_dist: f64) -> Option<i64> {
    if history.len() < 2 {
        return None;
    }

    // x = days since the first reading, y = radar distance (cm).
    let t0 = parse_ts(&history[0].ts)?;
    let mut xs = Vec::with_capacity(history.len());
    let mut ys = Vec::with_capacity(history.len());
    for r in history {
        xs.push((parse_ts(&r.ts)? - t0) as f64 / 86_400.0);
        ys.push(r.lvl);
    }

    let n = history.len() as f64;
    let mean_x = xs.iter().sum::<f64>() / n;
    let mean_y = ys.iter().sum::<f64>() / n;

    let mut sxx = 0.0;
    let mut sxy = 0.0;
    for i in 0..history.len() {
        let dx = xs[i] - mean_x;
        sxx += dx * dx;
        sxy += dx * (ys[i] - mean_y);
    }
    if sxx == 0.0 {
        // All readings share a timestamp; no rate to fit.
        return None;
    }

    // slope is cm/day; while the tank fills the distance shrinks, so a filling
    // trend is a negative slope. fill_rate is the positive cm/day it closes in
    // on `full_dist`.
    let slope = sxy / sxx;
    let fill_rate = -slope;
    if fill_rate <= 0.0 {
        return None;
    }

    let current = history.last().map(|r| r.lvl)?;
    let days = (current - full_dist) / fill_rate;
    Some(days.round().max(0.0) as i64)
}

/// Parses an RFC3339 timestamp to epoch seconds. Returns `None` on a malformed
/// timestamp so a single bad record degrades the projection rather than panicking.
fn parse_ts(ts: &str) -> Option<i64> {
    chrono::DateTime::parse_from_rfc3339(ts)
        .ok()
        .map(|dt| dt.timestamp())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rec(ts: &str, lvl: f64) -> ReadingRecord {
        ReadingRecord {
            ts: ts.into(),
            lvl,
            bat: 83,
        }
    }

    #[test]
    fn detects_a_pump_out_from_a_large_fullness_drop() {
        // Tank was ~80% full, then emptied to ~10%: a 70-point drop.
        assert!(is_pump_out(80, 10, 25));
    }

    #[test]
    fn normal_fluctuations_are_not_pump_outs() {
        // A few points of noise/refilling must not reset the baseline.
        assert!(!is_pump_out(20, 18, 25));
        assert!(!is_pump_out(18, 22, 25)); // filling slightly: negative drop
    }

    #[test]
    fn projects_days_left_from_a_steady_fill_rate() {
        // Distance falls 10 cm/day over five days (150 → 110). From the last
        // reading (110 cm) to full (40 cm) is 70 cm → 7 days at 10 cm/day.
        let history = vec![
            rec("2026-06-01T00:00:00+02:00", 150.0),
            rec("2026-06-02T00:00:00+02:00", 140.0),
            rec("2026-06-03T00:00:00+02:00", 130.0),
            rec("2026-06-04T00:00:00+02:00", 120.0),
            rec("2026-06-05T00:00:00+02:00", 110.0),
        ];
        assert_eq!(days_left(&history, 40.0), Some(7));
    }

    #[test]
    fn no_estimate_without_enough_history() {
        let history = vec![rec("2026-06-01T00:00:00+02:00", 150.0)];
        assert_eq!(days_left(&history, 40.0), None);
    }

    #[test]
    fn no_estimate_when_the_tank_is_not_filling() {
        // Distance growing (tank emptying) gives no days-to-full estimate.
        let history = vec![
            rec("2026-06-01T00:00:00+02:00", 110.0),
            rec("2026-06-02T00:00:00+02:00", 120.0),
            rec("2026-06-03T00:00:00+02:00", 130.0),
        ];
        assert_eq!(days_left(&history, 40.0), None);
    }
}
