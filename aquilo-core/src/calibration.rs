//! Tank calibration: the two distances that map a radar reading to a fill level.
//!
//! `lvl` is the radar distance to the liquid surface (cm); a *smaller* distance
//! means a *fuller* tank. `FULL_DIST` is the distance when the tank is full,
//! `EMPTY_DIST` the distance when empty. Both are tank-specific and live in
//! config (PRD: Configuration / user story 12). The defaults are the values that
//! reproduce the captured vendor samples.

use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, PartialEq, Deserialize, Serialize)]
pub struct Calibration {
    /// Radar distance (cm) at which the tank reads full. ~40 for the sample tank.
    #[serde(default = "default_full_dist")]
    pub full_dist: f64,
    /// Radar distance (cm) at which the tank reads empty. ~178 for the sample tank.
    #[serde(default = "default_empty_dist")]
    pub empty_dist: f64,
}

fn default_full_dist() -> f64 {
    40.0
}

fn default_empty_dist() -> f64 {
    178.0
}

impl Default for Calibration {
    fn default() -> Self {
        Calibration {
            full_dist: default_full_dist(),
            empty_dist: default_empty_dist(),
        }
    }
}

impl Calibration {
    /// Fill percentage, rounded to the nearest integer and clamped to 0–100.
    ///
    /// `pct = round((EMPTY_DIST - lvl) / (EMPTY_DIST - FULL_DIST) * 100)`
    pub fn pct(&self, level_cm: f64) -> i64 {
        let span = self.empty_dist - self.full_dist;
        let pct = (self.empty_dist - level_cm) / span * 100.0;
        (pct.round() as i64).clamp(0, 100)
    }

    /// Distance (cm) the level still has to fall to reach full, rounded to the
    /// nearest integer. `lvlToFull = round(lvl - FULL_DIST)`.
    pub fn lvl_to_full(&self, level_cm: f64) -> i64 {
        (level_cm - self.full_dist).round() as i64
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The captured vendor samples: `(lvl, pct, lvlToFull)`. `lvlToFull` for the
    /// 152.8 sample (113) is what pins rounding as *round-half-up*, not truncation
    /// — 112.8 rounds to 113.
    const SAMPLES: &[(f64, i64, i64)] = &[(152.8, 18, 113), (150.4, 20, 110), (150.2, 20, 110)];

    #[test]
    fn reproduces_captured_samples() {
        let cal = Calibration::default();
        for &(lvl, pct, lvl_to_full) in SAMPLES {
            assert_eq!(cal.pct(lvl), pct, "pct for lvl {lvl}");
            assert_eq!(cal.lvl_to_full(lvl), lvl_to_full, "lvlToFull for lvl {lvl}");
        }
    }

    #[test]
    fn calibration_drives_the_formulas() {
        let lvl = 150.2;
        let base = Calibration::default();
        // A deeper "empty" distance stretches the scale, so the same reading reads
        // as a fuller tank; moving "full" closer changes lvlToFull in lockstep.
        let stretched = Calibration {
            full_dist: 30.0,
            empty_dist: 200.0,
        };
        assert_ne!(stretched.pct(lvl), base.pct(lvl));
        assert_eq!(stretched.pct(lvl), 29); // (200-150.2)/170*100 = 29.3 -> 29
        assert_eq!(stretched.lvl_to_full(lvl), 120); // 150.2 - 30 = 120.2 -> 120
    }

    #[test]
    fn clamps_percentage_to_0_100() {
        let cal = Calibration::default();
        assert_eq!(cal.pct(10.0), 100); // below full distance -> overfull, clamped
        assert_eq!(cal.pct(250.0), 0); // beyond empty distance -> clamped
    }
}
