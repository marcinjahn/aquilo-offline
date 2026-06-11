//! The one place the wall clock is read. `aquilo-core` takes time as an argument
//! so it stays deterministic; the binary stamps readings with this.

/// Current local time as RFC3339 with offset (e.g. `2026-06-10T20:44:35+02:00`),
/// matching the `lstRead` the vendor cloud stamps.
pub fn now_rfc3339() -> String {
    chrono::Local::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, false)
}
