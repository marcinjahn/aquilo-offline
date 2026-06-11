//! Battery voltage → percentage curve.
//!
//! We have a single captured calibration point so far (`3770 mV → 83%`), so this
//! is deliberately a coarse approximation: a piecewise-linear interpolation across
//! a small breakpoint table anchored on that point, with reasonable Li-ion
//! endpoints. The table is refined as more `mV→%` samples are gathered (PRD: open
//! data gaps). The curve is monotonic and clamped to 0–100.

/// A monotonic mV→% mapping defined by breakpoints sorted ascending by voltage.
#[derive(Clone, Debug)]
pub struct BatteryCurve {
    /// `(millivolts, percent)` breakpoints, ascending by voltage.
    points: Vec<(i64, i64)>,
}

impl Default for BatteryCurve {
    fn default() -> Self {
        // 3300 mV ≈ empty (Li-ion cutoff under load), 4200 mV ≈ full, and the one
        // captured point (3770 → 83) in between.
        BatteryCurve {
            points: vec![(3300, 0), (3770, 83), (4200, 100)],
        }
    }
}

impl BatteryCurve {
    /// Builds a curve from explicit breakpoints. They are sorted ascending by
    /// voltage so callers needn't pre-sort.
    pub fn new(mut points: Vec<(i64, i64)>) -> Self {
        points.sort_by_key(|&(mv, _)| mv);
        BatteryCurve { points }
    }

    /// Maps a battery voltage (mV) to a percentage, interpolating linearly between
    /// breakpoints and clamping below the first / above the last.
    pub fn percent(&self, mv: i64) -> i64 {
        let pts = &self.points;
        if mv <= pts[0].0 {
            return pts[0].1;
        }
        if mv >= pts[pts.len() - 1].0 {
            return pts[pts.len() - 1].1;
        }
        for w in pts.windows(2) {
            let (lo_mv, lo_pct) = w[0];
            let (hi_mv, hi_pct) = w[1];
            if mv <= hi_mv {
                let t = (mv - lo_mv) as f64 / (hi_mv - lo_mv) as f64;
                let pct = lo_pct as f64 + t * (hi_pct - lo_pct) as f64;
                return pct.round() as i64;
            }
        }
        // Unreachable: the bounds checks above cover everything outside the windows.
        pts[pts.len() - 1].1
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reproduces_the_captured_point() {
        assert_eq!(BatteryCurve::default().percent(3770), 83);
    }

    #[test]
    fn clamps_outside_the_table() {
        let curve = BatteryCurve::default();
        assert_eq!(curve.percent(3000), 0);
        assert_eq!(curve.percent(5000), 100);
    }

    #[test]
    fn interpolates_and_is_monotonic() {
        let curve = BatteryCurve::default();
        // Halfway between 3300 (0%) and 3770 (83%).
        assert_eq!(curve.percent(3535), 42);
        let mut last = -1;
        for mv in (3200..=4300).step_by(50) {
            let pct = curve.percent(mv);
            assert!(pct >= last, "curve must not decrease at {mv} mV");
            last = pct;
        }
    }

    #[test]
    fn accepts_unsorted_custom_breakpoints() {
        let curve = BatteryCurve::new(vec![(4000, 100), (3000, 0)]);
        assert_eq!(curve.percent(3500), 50);
    }
}
