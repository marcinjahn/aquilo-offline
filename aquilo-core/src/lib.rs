//! Pure, deterministic protocol/compute core for the Aquilo offline server. It
//! parses the device's raw `/read` payload and computes the vendor `/state` JSON
//! from configurable calibration and a battery curve. Time is injected by callers
//! rather than read from a clock, so every computation is reproducible under test.

pub mod battery;
pub mod calibration;
pub mod history;
pub mod reading;
pub mod state;

pub use battery::BatteryCurve;
pub use calibration::Calibration;
pub use history::ReadingRecord;
pub use reading::{RawFrame, Reading};
pub use state::{SensorState, StaticFields};
