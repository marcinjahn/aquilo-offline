//! MQTT topic strings, all templated from the receiver id (`<rid>`).

pub struct Topics {
    /// Retained firmware version the device checks against (avoids OTA).
    pub version: String,
    /// Retained computed state; the device caches and serves it on HTTP `/state`.
    pub state: String,
    /// Retained radar/radio params for the sensor node.
    pub radar_params: String,
    /// Raw measurement published by the device.
    pub read: String,
    /// Version/MAC announcement published by the device on connect.
    pub log: String,
    /// `online`/`offline` published by the device.
    pub connection: String,
    /// App-level keepalive published by the server.
    pub ping: String,
}

impl Topics {
    pub fn new(rid: &str) -> Self {
        Self {
            version: format!("/version/czujnik_szamba/{rid}"),
            state: format!("/users/{rid}/state"),
            radar_params: format!("/users/{rid}/receivers/{rid}/radarParams"),
            read: format!("/users/{rid}/sensors/{rid}/read"),
            log: format!("/users/{rid}/sensors/{rid}/log"),
            connection: format!("/users/{rid}/sensors/{rid}/connection"),
            ping: "/ping".to_string(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn templates_from_receiver_id() {
        let t = Topics::new("ae83fc");
        assert_eq!(t.version, "/version/czujnik_szamba/ae83fc");
        assert_eq!(t.state, "/users/ae83fc/state");
        assert_eq!(t.radar_params, "/users/ae83fc/receivers/ae83fc/radarParams");
        assert_eq!(t.read, "/users/ae83fc/sensors/ae83fc/read");
        assert_eq!(t.log, "/users/ae83fc/sensors/ae83fc/log");
        assert_eq!(t.connection, "/users/ae83fc/sensors/ae83fc/connection");
    }
}
