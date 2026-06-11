//! Minimal offline broker that stands in for the Aquilo vendor cloud: it keeps the
//! device connected and republishes a retained `/state`, so the device's own HTTP
//! `/state` keeps serving with the real internet cut.

pub mod broker;
pub mod config;
pub mod reading;
pub mod server;
pub mod state;
pub mod topics;
