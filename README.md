# Aquilo offline server

Run an [Aquilo WiFi septic-tank liquid-level sensor](https://aquilo.pl/produkt/aquilo-wifi-czujnik-poziomu-cieczy-bezprzewodowy-mikrofalowy/)
completely offline (no internet, no vendor cloud) while keeping the local
experience you already have. The device's own `http://<device-ip>/state` endpoint
keeps serving correct, computed data, just as before.

The problem with the stock setup is that the Aquilo receiver is basically a dumb
display. It shows whatever state the vendor cloud (`mqtt.aquilo.cloud`) computes
and pushes back to it over MQTT. Cut its internet and even its local `/state`
endpoint stops responding. This project is a small Rust server that stands in for
that cloud on your own network. The device reaches it through a DNS rewrite, the
server keeps the session alive and recomputes the derived state on every reading,
and `/state` keeps working with no internet and no changes to the device.

It ships as a Home Assistant add-on that connects to a Mosquitto broker, and it's
light enough to run on a Raspberry Pi.

Only tested with the **WiFi** sensor variant. The LTE variant and multi-sensor
setups aren't covered. The config templating leaves room for them, but that path
is untested.

## Why this exists

- **Your sensor shouldn't die when the vendor does.** Because the cloud computes
  values like `pct` and `daysLeft` server-side, the day Aquilo shuts down its
  servers your paid hardware turns into a brick, and `/state` goes silent even on
  your own LAN. This server lets the device outlive the company.
- **Privacy.** There's no reason a tank-level reading needs to leave your
  property. Offline, it doesn't.
- **No vendor lock-in.** You own the whole data path: your broker, your
  calibration, your history.
- **Less attack surface.** Fewer always-on outbound connections from a
  poorly-secured ESP device on your network is simply safer.

## How it works

```
  radar sensor node ──radio──▶ receiver/gateway ──MQTT──▶ Mosquitto ──▶ aquilo-server
   (battery, outdoor)          (always-on, :80 /state)    (HA add-on)    (this project)
                                       ▲                                       │
                              HTTP GET │ /state          recomputed retained   │
                              (Home Assistant            /state republished ───┘
                               RESTful sensor)
```

- The receiver connects over plaintext MQTT on port 1883 using fixed firmware
  credentials. A DNS rewrite points `mqtt.aquilo.cloud` at your Home Assistant
  host, so the device lands on your Mosquitto broker instead of the cloud.
- `aquilo-server` is a client of Mosquitto, not a second broker. It replays the
  retained connect-time messages, answers keepalives, and on every raw reading it
  computes the derived state (`pct`, `lvlToFull`, `daysLeft`, battery %,
  `lstEmpty`) and republishes the retained `/state`.
- The receiver caches that and serves it over HTTP at `/state`, unchanged. All
  data consumption goes through the device's `/state`. The add-on creates no Home
  Assistant entities and exposes no API of its own; you surface the data with Home
  Assistant's built-in RESTful sensor.

## Setup (Home Assistant)

The full copy-paste setup lives in
[`addon/aquilo-server/DOCS.md`](addon/aquilo-server/DOCS.md). The outline:

1. **Install the Mosquitto broker add-on** and this repo's add-on (Settings →
   Add-ons → ⋮ → **Repositories** → add this repo's URL → install **Aquilo offline
   server**).
2. **Recover your device identity** with the one-time [onboarding](#onboarding)
   step. This gives you `receiver_id`, `sensor_id`, the MQTT user/password, and
   firmware. Enter those plus your tank calibration into the add-on options.
3. **Add the device's MQTT login to Mosquitto** (the fixed firmware credentials).
4. **Redirect `mqtt.aquilo.cloud` to your Home Assistant host** (see
   [DNS rewrite](#dns-rewrite)).
5. **Block the device's remaining internet access** (see
   [going fully offline](#going-fully-offline)).
6. **Surface the data** with a RESTful sensor pointed at `/state` (config in
   DOCS.md).

Calibration values (`full_dist` / `empty_dist`) and all other device-specific
values are add-on options, and changes take effect on restart. State (history,
`lstEmpty`, and the last `/state`) lives in `/data`, so it survives restarts,
reboots, and updates.

### Onboarding

The binary recovers your device's IDs, credentials, firmware, and the cloud's
retained messages, then writes the initial `config.toml` and state file, so you
never have to hand-edit topics or IDs. There are two modes:

- **`learn`** is a transparent MQTT proxy to the real cloud (reached by IP to avoid
  the DNS-rewrite loop). This is the most faithful seed: it records the device's
  CONNECT credentials and the cloud's actual retained `/state` and `radarParams`.
  Run it once, before you cut off the cloud.
- **`observe`** needs no cloud at all. A tiny stand-in broker completes the
  handshake itself, seeds retained messages from documented defaults, and recovers
  the device identity from what the device announces on connect. Use this when the
  cloud is already gone (see [Backup & recovery](#backup--recovery)).

```sh
cargo run -p aquilo-server -- learn   --config config.toml --data-dir data  # proxy via real cloud
cargo run -p aquilo-server -- observe --config config.toml --data-dir data  # cloud-free
cargo run -p aquilo-server -- serve   --config config.toml                  # normal operation (add-on default)
```

### DNS rewrite

Point the vendor's MQTT hostname at your Home Assistant host. In AdGuard Home
(Filters → DNS rewrites) or your router's local DNS, add:

```
mqtt.aquilo.cloud  →  <your HA host IP>
```

This is the only outbound dependency the device needs satisfied locally. It's a
manual step on your own router or AdGuard; the add-on doesn't automate it.

### Going fully offline

Once `serve` is running and `/state` shows fresh data through the rewrite, you can
cut the device's remaining internet access at your firewall:

| Endpoint                                                           | Action                          | Why                                                                                    |
| ------------------------------------------------------------------ | ------------------------------- | -------------------------------------------------------------------------------------- |
| `mqtt.aquilo.cloud`                                                | **Redirect** (DNS rewrite)      | The only real dependency, now served locally.                                          |
| `time.aws.com` (NTP)                                               | **Block** (optionally redirect) | Not required, even on a cold boot. See the [firewall test](#firewall-test).            |
| `c2n0py5cened4k.credentials.iot.us-east-1.amazonaws.com` (AWS IoT) | **Block, keep blocked**         | Provisioning-only mTLS endpoint, never touched in operation and can't be impersonated. |

## Backup & recovery

- **Home Assistant native backup (primary).** All state lives in `/data` and the
  options ride along with the add-on, so any HA backup that includes this add-on
  captures both. `config.yaml` sets `backup: cold`, so the Supervisor stops the
  add-on while `/data` is archived and it's never captured half-written. To
  recover, restore the backup and the add-on comes back with full history and
  calibration. Keep your backups off the Pi.
- **No backup, cloud gone.** The device's identity can be recovered purely from
  what it announces on connect. Run **`observe`** onboarding (no internet, no
  proxy) and it fills in `radarParams` and calibration from defaults, bringing live
  data back immediately. You only lose the reading history and the `lstEmpty`
  baseline, and both self-heal as new readings arrive (`lstEmpty` can also be set
  manually).

## Project layout

| Crate / dir           | Role                                                                                                                                                                      |
| --------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `aquilo-core`         | Pure, tested logic: raw-reading parser, state computer, pump-out detector, days-left estimator, battery curve, message builder. Time is injected for deterministic tests. |
| `aquilo-store`        | Persistence trait plus an atomic JSON-file implementation (history, `lstEmpty`, last state, cached config).                                                               |
| `aquilo-server`       | The binary: `serve` / `learn` / `observe` modes, MQTT client (`rumqttc`), topic templating, config.                                                                       |
| `addon/aquilo-server` | Home Assistant add-on packaging (Dockerfile, `config.yaml`, `build.yaml`, `DOCS.md`).                                                                                     |
| `capture/`            | Original Node reverse-engineering tooling, kept as the behavioral reference (see [Capture tooling](#capture-tooling)).                                                    |

`cargo test` covers the core formulas against captured samples, persistence
round-trips, and an in-process-broker integration test that exercises the full
connect → handshake → read round-trip without the physical device.

---

# Reference: reverse-engineering & protocol

This section documents the protocol the server reimplements. You don't need it for
setup; it's here for maintenance and debugging.

## Hardware / architecture

- **Sensor node `ae5058`** is a battery-powered radar liquid-level sensor that sits
  outdoors. It measures the distance to the liquid surface and transmits over
  radio. Identified as `node-4`.
- **Receiver `ae83fc`** is an always-on indoor gateway at `172.20.0.239`
  (MAC `D8:BC:38:AE:83:FC`, ESP-based, firmware `1.7.1.9_sh_en`). It receives the
  radio data, talks to the cloud over MQTT, and serves a local HTTP `/state`. It
  computes nothing; it just displays whatever state the cloud sends.

All derived values (`pct`, `daysLeft`, `lvlToFull`, `lstEmpty`) are computed
server-side in the cloud. For reference, the development LAN is `172.20.0.0/24`,
the dev machine is `172.20.0.146`, and the device is `172.20.0.239`.

## Cloud endpoints

| Domain                                                   | Resolves to                | Purpose                               | Notes                                                        |
| -------------------------------------------------------- | -------------------------- | ------------------------------------- | ------------------------------------------------------------ |
| `mqtt.aquilo.cloud`                                      | `57.128.198.238` (OVH VPS) | MQTT broker: telemetry up, state down | Ports 1883 plain, 8883 TLS, 443. The device uses plain 1883. |
| `c2n0py5cened4k.credentials.iot.us-east-1.amazonaws.com` | AWS                        | AWS IoT credentials provider          | Never touched in normal operation, so safe to block.         |
| `time.aws.com`                                           | AWS                        | NTP time sync                         | Not required; see the firewall test. No local NTP needed.    |

The MQTT link is the only outbound dependency, and it's what this project stands in
for.

### Firewall test

With the transparent proxy running (device → laptop → real broker) and the
firewall blocking the device's WAN access (cutting both `time.aws.com` and the AWS
IoT endpoint), the device was cold-rebooted. It reconnected, completed the full
handshake, received all 3 retained messages, entered steady state
(`PINGREQ`/`PINGRESP` every ~45 s), and kept `/state` serving. So NTP and the AWS
IoT provider aren't needed, even on a cold boot. The only thing left to prove was
that our own broker can stand in for the proxy, which is what `serve` does.

## Local HTTP API (receiver, port 80)

This is a minimal embedded server. It chokes on rapid back-to-back requests, so hit
it one at a time (`Connection: close`, roughly 1.5 s apart). Unknown paths return
`404 Not found: /<path>`.

- `GET /` returns the text `Aquilo liquid level sensor`
- `GET /state` returns cached state JSON (the same content the cloud pushes over MQTT):

```json
{
  "sensors": [
    {
      "id": "ae5058",
      "lvl": 150.2,
      "pct": 20,
      "bat": 83,
      "lstRead": "2026-06-10T20:44:35+02:00",
      "lstEmpty": "2026-05-30T00:17:19+02:00",
      "daysLeft": 51,
      "name": "ae5058",
      "lvlToFull": 110
    }
  ],
  "from": "node-4"
}
```

## MQTT protocol

Connection details: clientId `CieczSensor<rid>` (observed as `CieczSensorae83fc`),
user `<rid>`, a per-device numeric password, plaintext on port 1883.

On connect the device publishes `online` plus some log lines, subscribes to 10
topics, and the server delivers the 3 retained messages it needs:

| Topic                                        | Retained payload                          |
| -------------------------------------------- | ----------------------------------------- |
| `/version/czujnik_szamba/ae83fc`             | `1.7.1.9_sh_en` (same fw → no OTA)        |
| `/users/ae83fc/state`                        | last known state JSON (above)             |
| `/users/ae83fc/receivers/ae83fc/radarParams` | `{"sensor":"ae5058","skip":9,"repeat":9}` |

Device → server topics:

- `/users/ae83fc/sensors/ae83fc/connection` → `online` / `offline`
- `/users/ae83fc/sensors/ae83fc/log` → version and MAC (the firmware string is
  learned from this and echoed back, so it needs no config)
- `/users/ae83fc/sensors/ae83fc/read` → the raw measurement (below)

Device subscriptions: `/version/czujnik_szamba/ae83fc`, `/ping`,
`/users/ae83fc/state`, `/users/ae83fc/{restart,reset,cp,sensorCommand,updateSensor}`,
`/users/ae83fc/receivers/ae83fc/{params/set,radarParams}`.

Keepalive: the device sends `PINGREQ` roughly every 45 s, and the server also
publishes an app-level `/ping` (around every 20 min). With proper state replies and
the 3 retained messages, the device stays connected for hours. A silent broker
makes it cycle offline → reconnect about every 40 min, so correct replies are
essential. `params/set` is never required.

### Measurement round-trip (the core exchange)

The device publishes to `/users/ae83fc/sensors/ae83fc/read`:

```json
{
  "salt": "<nonce>",
  "raw": "_e23_ae5058_1502_3770_59_0_73044_72976_1_2_1_0_0_0",
  "v": "e23",
  "sensor": "ae5058",
  "read1": 15020,
  "battery": "3770",
  "crc": true,
  "amp1": 59,
  "temp": 0,
  "readNo": 73044,
  "lastSent": 72976,
  "rev": 1,
  "r": 2,
  "rssi": -75,
  "wifi": -80,
  "snr": 5
}
```

The server replies roughly 90 ms later on the retained `/users/ae83fc/state` with
the computed state. `salt` is an anti-replay nonce and isn't echoed back.

## Derived-value formulas (reverse-engineered)

`lvl` is the radar distance to the liquid surface, in cm. A smaller distance means
a fuller tank.

- `lvl = read1 / 100` (15020 → 150.2; the `raw` field `1502` is in mm)
- `lvlToFull = lvl - FULL_DIST`, with `FULL_DIST ≈ 40` cm
- `pct = round((EMPTY_DIST - lvl) / (EMPTY_DIST - FULL_DIST) * 100)`, `EMPTY_DIST ≈ 178` cm
- `daysLeft` is a fill-rate projection since `lstEmpty`
- `lstEmpty` is the timestamp of the last pump-out (detected as a large level drop)
- `bat` is a battery mV → % curve (`3770 mV → 83%`; just one data point so far)

`FULL_DIST` and `EMPTY_DIST` are server-side tank config; offline they become our
own calibration constants (the `full_dist` / `empty_dist` add-on options).
`daysLeft` and `bat%` are reasonable approximations, not exact replicas of the
vendor's algorithm. Samples gathered so far: `(lvl 152.8 → pct 18)`,
`(lvl 150.4 → pct 20, lvlToFull 110)`, `(lvl 150.2 → pct 20, lvlToFull 110, daysLeft 51)`.

### Capture tooling

The Node tooling in `capture/` is kept around as diagnostics and as the behavioral
reference for the Rust `learn` mode and the protocol integration test:

- `broker.js` is an aedes broker that accepts the device and logs everything (no
  upstream).
- `proxy.js` is a transparent MQTT proxy to the real broker (upstream by IP to
  avoid the DNS loop) that decodes both directions. `UPSTREAM_HOST`/`PORT` and
  `LISTEN_PORT` are env-overridable.
- `capture.py` + `gen-cert.sh` is a self-signed TLS logger to test whether the AWS
  endpoints validate the server cert. Probably unneeded, since the device uses
  plaintext MQTT.
- `README.md` has run instructions.

## Open data gaps (non-blocking)

These are accuracy improvements only: the exact `/ping` cadence; more
`(lvl, pct, lvlToFull)` samples to pin down `FULL_DIST` / `EMPTY_DIST`; and more
battery `mV → %` points.
