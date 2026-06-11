//! Offline broker that stands in for the Aquilo vendor cloud: it keeps the device
//! connected and, on each raw reading, republishes a calibrated retained `/state`
//! (computed by `aquilo-core`), so the device's own HTTP `/state` keeps serving
//! with the real internet cut.

pub mod broker;
pub mod clock;
pub mod config;
pub mod server;
pub mod topics;
