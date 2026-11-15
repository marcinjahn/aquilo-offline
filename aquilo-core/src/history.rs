//! Reading history and the history-dependent projections the vendor cloud
//! computed for us: pump-out detection (a confirmed sharp fullness drop starts a
//! new fill cycle and resets the `lstEmpty` baseline) and the `daysLeft`
//! projection from the fill rate since the cycle started.
//!
//! These are pure functions over [`ReadingRecord`]s; time enters only as the
//! RFC3339 timestamps already stored on the records, so the projections are
//! deterministic under test.

use serde::{Deserialize, Serialize};

use crate::calibration::Calibration;

/// A fill cycle younger than this has too little level change to out-weigh radar
/// noise (a few cm between consecutive readings), so its own rate isn't trusted
/// yet and the previous cycle's rate is used instead.
pub const MIN_CYCLE_DAYS: f64 = 3.0;

/// How many of the latest readings are median-filtered into the current level,
/// so a single radar glitch can't swing the projection.
const SMOOTHING_WINDOW: usize = 5;

/// One stored reading: when it was received (RFC3339 with offset), the radar
/// distance to the surface (cm; smaller = fuller), and the battery percentage at
/// that time. This is the rolling history the projections run over.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ReadingRecord {
    pub ts: String,
    pub lvl: f64,
    pub bat: i64,
}

/// What a sharp fullness drop turned out to be, judged by the reading after it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Drop {
    /// No sharp drop between the reading before and the suspected one.
    None,
    /// The drop held on the next reading: the tank was emptied.
    PumpOut,
    /// The level came straight back: the suspected reading was a radar glitch.
    Outlier,
}

/// Classifies the middle of three consecutive readings (`before`, `suspect`,
/// `after`). A fullness fall of at least `drop_threshold_pct` points from
/// `before` to `suspect` is a pump-out only if `after` stays that far below
/// `before` too; a single low reading is a glitch, not an emptied tank.
pub fn classify_drop(
    before: &ReadingRecord,
    suspect: &ReadingRecord,
    after: &ReadingRecord,
    calibration: &Calibration,
    drop_threshold_pct: i64,
) -> Drop {
    let pct_before = calibration.pct(before.lvl);
    if pct_before - calibration.pct(suspect.lvl) < drop_threshold_pct {
        Drop::None
    } else if pct_before - calibration.pct(after.lvl) >= drop_threshold_pct {
        Drop::PumpOut
    } else {
        Drop::Outlier
    }
}

/// The fill rate (cm/day) of the current cycle: how far the level has risen
/// from the cycle's first reading to the smoothed current reading, over the time
/// between them.
///
/// Returns `None` while the cycle is younger than [`MIN_CYCLE_DAYS`], when the
/// level isn't rising, or on a malformed timestamp.
pub fn fill_rate(cycle_start: &ReadingRecord, history: &[ReadingRecord]) -> Option<f64> {
    let current = smoothed_reading(history)?;
    let elapsed_days = days_between(cycle_start, current)?;
    if elapsed_days < MIN_CYCLE_DAYS {
        return None;
    }

    // The radar distance shrinks as the tank fills.
    let rate = (cycle_start.lvl - current.lvl) / elapsed_days;
    (rate > 0.0).then_some(rate)
}

/// Days from the latest reading until the level reaches `full_dist` at
/// `fill_rate` cm/day, projected from the smoothed current reading. Zero once
/// the tank is at or past full.
pub fn days_left(history: &[ReadingRecord], full_dist: f64, fill_rate: f64) -> Option<i64> {
    let current = smoothed_reading(history)?;
    let since_current = days_between(current, history.last()?)?;
    let days = (current.lvl - full_dist) / fill_rate - since_current;
    Some(days.round().max(0.0) as i64)
}

/// The median-level reading among the latest [`SMOOTHING_WINDOW`]. Returning the
/// reading itself, not just its level, keeps its timestamp, so a level that lags
/// the newest reading on a rising trend is not mistaken for a slower fill.
fn smoothed_reading(history: &[ReadingRecord]) -> Option<&ReadingRecord> {
    let start = history.len().saturating_sub(SMOOTHING_WINDOW);
    let mut window: Vec<&ReadingRecord> = history[start..].iter().collect();
    window.sort_by(|a, b| a.lvl.total_cmp(&b.lvl));
    window.get(window.len() / 2).copied()
}

fn days_between(from: &ReadingRecord, to: &ReadingRecord) -> Option<f64> {
    Some((parse_ts(&to.ts)? - parse_ts(&from.ts)?) as f64 / 86_400.0)
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

    fn day(d: u32, lvl: f64) -> ReadingRecord {
        ReadingRecord {
            ts: format!("2026-06-{d:02}T00:00:00+02:00"),
            lvl,
            bat: 83,
        }
    }

    /// A tank filling 4 cm/day from empty (178 cm), one reading a day.
    fn filling(days: u32) -> Vec<ReadingRecord> {
        (1..=days)
            .map(|d| day(d, 178.0 - 4.0 * (d - 1) as f64))
            .collect()
    }

    fn classify(before: f64, suspect: f64, after: f64) -> Drop {
        classify_drop(
            &day(1, before),
            &day(2, suspect),
            &day(3, after),
            &Calibration::default(),
            25,
        )
    }

    #[test]
    fn a_drop_that_holds_is_a_pump_out() {
        // ~93% full, then emptied to ~6% and staying there.
        assert_eq!(classify(50.0, 170.0, 169.0), Drop::PumpOut);
    }

    #[test]
    fn a_drop_that_bounces_back_is_a_glitch() {
        assert_eq!(classify(50.0, 170.0, 50.5), Drop::Outlier);
    }

    #[test]
    fn normal_fluctuations_are_not_drops() {
        assert_eq!(classify(150.2, 152.8, 150.4), Drop::None);
    }

    #[test]
    fn fill_rate_is_the_rise_since_the_cycle_start() {
        let history = filling(11);
        assert_eq!(fill_rate(&history[0], &history), Some(4.0));
    }

    #[test]
    fn a_young_cycle_has_no_rate_of_its_own() {
        let history = filling(2);
        assert_eq!(fill_rate(&history[0], &history), None);
    }

    #[test]
    fn a_level_that_isnt_rising_has_no_rate() {
        let history: Vec<_> = (1..=5).map(|d| day(d, 120.0 + d as f64)).collect();
        assert_eq!(fill_rate(&history[0], &history), None);
    }

    #[test]
    fn a_single_glitch_does_not_move_the_rate() {
        let mut history = filling(11);
        history.push(day(12, 5.0));
        // The median of the last five (150, 146, 142, 138, 5) is 142, on the trend.
        assert_eq!(fill_rate(&history[0], &history), Some(4.0));
    }

    #[test]
    fn a_tank_plateaued_near_full_reads_as_due() {
        // Filled ~4.4 cm/day for a month, then sat at 45 cm (outlet level) for 10 days.
        let mut history: Vec<_> = (1..=30)
            .map(|d| day(d, 178.0 - 4.4 * (d - 1) as f64))
            .collect();
        history.extend((1..=10).map(|d| {
            let mut r = day(d, 45.0);
            r.ts = format!("2026-07-{d:02}T00:00:00+02:00");
            r
        }));
        let rate = fill_rate(&history[0], &history).unwrap();
        assert_eq!(days_left(&history, 40.0, rate), Some(0));
    }

    #[test]
    fn projects_days_from_the_smoothed_level() {
        let history = vec![day(1, 112.0), day(2, 110.0), day(3, 108.0)];
        // 110 cm smoothed a day before the latest: 7 days to full from then, 6 from now.
        assert_eq!(days_left(&history, 40.0, 10.0), Some(6));
    }

    #[test]
    fn a_full_tank_has_zero_days_left() {
        let history = vec![day(1, 38.0), day(2, 37.0), day(3, 39.0)];
        assert_eq!(days_left(&history, 40.0, 2.5), Some(0));
    }
}
