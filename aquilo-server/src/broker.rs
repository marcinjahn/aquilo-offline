//! In-process `rumqttd` broker. The device connects here on plaintext :1883 with
//! its fixed credentials; the server's own logic connects as a separate `rumqttc`
//! client (the production code path). In the HA deployment this broker is replaced
//! by the external Mosquitto add-on, but the client code stays identical.

use std::collections::HashMap;

use anyhow::Context;
use rumqttd::{Broker, Config as BrokerConfig, ConnectionSettings, RouterConfig, ServerSettings};

use crate::config::Config;

/// Credentials the server's own MQTT client uses to connect to the broker.
pub const INTERNAL_USER: &str = "aquilo-server";
pub const INTERNAL_PASS: &str = "aquilo-internal";

fn broker_config(cfg: &Config) -> anyhow::Result<BrokerConfig> {
    // Both the device and our own client must authenticate against this map.
    let mut auth = HashMap::new();
    auth.insert(cfg.mqtt_user.clone(), cfg.mqtt_pass.clone());
    auth.insert(INTERNAL_USER.to_string(), INTERNAL_PASS.to_string());

    let connections = ConnectionSettings {
        connection_timeout_ms: 60_000,
        max_payload_size: 20480,
        // Must comfortably exceed the count of retained messages delivered on a
        // single subscribe (rumqttd caps that batch at the inflight slots).
        max_inflight_count: 100,
        auth: Some(auth),
        external_auth: None,
        dynamic_filters: true,
    };

    let listen = format!("{}:{}", cfg.bind_addr, cfg.listen_port)
        .parse()
        .with_context(|| format!("invalid bind address {}:{}", cfg.bind_addr, cfg.listen_port))?;

    let server = ServerSettings {
        name: "v4-1".to_string(),
        listen,
        tls: None,
        next_connection_delay_ms: 1,
        connections,
    };

    let mut v4 = HashMap::new();
    v4.insert("v4-1".to_string(), server);

    let router = RouterConfig {
        max_connections: 10010,
        max_outgoing_packet_count: 200,
        max_segment_size: 104_857_600,
        max_segment_count: 10,
        custom_segment: None,
        initialized_filters: None,
        shared_subscriptions_strategy: Default::default(),
    };

    Ok(BrokerConfig {
        id: 0,
        router,
        v4: Some(v4),
        v5: None,
        ws: None,
        cluster: None,
        console: None,
        bridge: None,
        prometheus: None,
        metrics: None,
    })
}

/// Starts the broker on a dedicated thread. `Broker::start` blocks (it joins its
/// server threads and manages its own tokio runtimes), so it cannot share our
/// async runtime.
pub fn spawn(cfg: &Config) -> anyhow::Result<()> {
    let config = broker_config(cfg)?;
    std::thread::Builder::new()
        .name("aquilo-broker".to_string())
        .spawn(move || {
            let mut broker = Broker::new(config);
            if let Err(e) = broker.start() {
                tracing::error!(error = ?e, "broker stopped");
            }
        })
        .context("spawning broker thread")?;
    Ok(())
}
