# Aquilo offline server

Run an **Aquilo WiFi septic-tank liquid-level sensor fully offline** — no internet,
no vendor cloud — while keeping the exact local experience you already have: the
device's own `http://<device-ip>/state` endpoint serving correct, computed data.

The Aquilo receiver is a dumb pipe: it displays whatever state the vendor cloud
(`mqtt.aquilo.cloud`) computes and pushes back over MQTT. Cut its internet and even
its **local** `/state` stops responding. This is a small Rust server that
impersonates that cloud on your own network. The device reaches it via a DNS
rewrite; the server keeps the session alive, recomputes the derived state on every
reading, and `/state` keeps working as before — zero internet, zero device changes.

Ships as a **Home Assistant add-on** (a Mosquitto-broker client), small enough for a
Raspberry Pi.

> Tested with the **WiFi** sensor variant only. Multi-sensor / multi-receiver setups
> are out of scope (config templating leaves room, but it is untested).

## Why this exists

An internet-connected sensor for a hole of sewage in your garden is a bad deal:

- **It dies when the vendor does.** A cloud that computes `pct` and `daysLeft`
  server-side means the day Aquilo shuts down its servers, your paid hardware turns
  into a brick — `/state` goes silent even on your own LAN. This server makes the
  device outlive the company.
- **Privacy.** There's no reason a tank-level reading should leave your property.
  Offline, it doesn't.
- **No vendor lock-in.** You own the data path end to end — your broker, your
  calibration, your history.
- **Security.** Fewer always-on outbound connections from a cheap ESP device on your
  network is simply less attack surface.

## How it works

```
  radar sensor node ──radio──▶ receiver/gateway ──MQTT──▶ Mosquitto ──▶ aquilo-server
   (battery, outdoor)          (always-on, :80 /state)    (HA add-on)    (this project)
                                       ▲                                       │
                              HTTP GET │ /state          recomputed retained   │
                              (Home Assistant            /state republished ───┘
                               RESTful sensor)
```

- The receiver connects over **plaintext MQTT :1883** with fixed firmware
  credentials. A DNS rewrite points `mqtt.aquilo.cloud` at your HA host, so it lands
  on your **Mosquitto** broker instead of the cloud.
- `aquilo-server` is a **client of Mosquitto** (not a second broker). It replays the
  retained connect-time messages, answers keepalives, and on every raw reading
  computes the derived state (`pct`, `lvlToFull`, `daysLeft`, battery %, `lstEmpty`)
  and republishes the retained `/state`.
- The receiver caches that and serves it on HTTP `/state`, unchanged. **All data
  consumption is through the device's `/state`** — the add-on creates no HA entities
  and exposes no API of its own. Surface the data with HA's built-in RESTful sensor.

## Setup (Home Assistant)

Full copy-paste setup is in **[`addon/aquilo-server/DOCS.md`](addon/aquilo-server/DOCS.md)**. Outline:

1. **Install the Mosquitto broker add-on** and this repo's add-on (Settings →
   Add-ons → ⋮ → **Repositories** → add this repo's URL → install **Aquilo offline
   server**).
2. **Recover your device identity** via one-time [onboarding](#onboarding) —
   `receiver_id`, `sensor_id`, MQTT user/pass, firmware. Enter them plus your tank
   calibration in the add-on options.
3. **Add the device's MQTT login to Mosquitto** (the fixed firmware credentials).
4. **Redirect `mqtt.aquilo.cloud` to your HA host** ([DNS rewrite](#dns-rewrite)).
5. **Block the device's other internet** ([going fully offline](#going-fully-offline)).
6. **Surface the data** with a RESTful sensor against `/state` (config in DOCS.md).
   Poll **gently** — one request at a time, `Connection: close`, ~60 s apart; the
   receiver's embedded HTTP server chokes on rapid requests, and readings arrive only
   a few times a day so nothing is lost.

Calibration (`full_dist` / `empty_dist`) and all device-specific values are add-on
options; changes take effect on restart. State (history, `lstEmpty`, last `/state`)
lives in `/data` and survives restarts, reboots, and updates.

### Onboarding

The binary recovers your device's IDs, credentials, firmware, and the cloud's
retained messages, then writes the initial `config.toml` + state file — so you never
hand-edit topics or IDs. Two modes:

- **`learn`** — a transparent MQTT proxy to the real cloud (reached by IP to dodge
  the DNS-rewrite loop). The most faithful seed: records the device's CONNECT
  credentials and the cloud's actual retained `/state` and `radarParams`. Use this
  **once, before** you cut the cloud.
- **`observe`** — needs **no cloud**. A tiny stand-in broker completes the handshake
  itself, seeds retained messages from documented defaults, and recovers identity
  from what the device announces on connect. Use this when the cloud is already gone
  (see [Backup & recovery](#backup--recovery)).

```sh
cargo run -p aquilo-server -- learn   --config config.toml --data-dir data  # proxy via real cloud
cargo run -p aquilo-server -- observe --config config.toml --data-dir data  # cloud-free
cargo run -p aquilo-server -- serve   --config config.toml                  # normal operation (add-on default)
```

### DNS rewrite

Point the vendor's MQTT hostname at your HA host. On AdGuard Home (Filters → DNS
rewrites) or your router's local DNS:

```
mqtt.aquilo.cloud  →  <your HA host IP>
```

This is the **only** outbound dependency the device needs satisfied locally — a
manual step on your own router/AdGuard; the add-on does not automate it.

### Going fully offline

Once `serve` is running and `/state` shows fresh data through the rewrite, cut the
device's remaining internet at your firewall:

| Endpoint                                                           | Action                          | Why                                                                                  |
| ------------------------------------------------------------------ | ------------------------------- | ------------------------------------------------------------------------------------ |
| `mqtt.aquilo.cloud`                                                | **Redirect** (DNS rewrite)      | The only real dependency; now served locally.                                        |
| `time.aws.com` (NTP)                                               | **Block** (optionally redirect) | Not required, even on cold boot — see [firewall test](#firewall-test-2026-06-10).    |
| `c2n0py5cened4k.credentials.iot.us-east-1.amazonaws.com` (AWS IoT) | **Block, keep blocked**         | Provisioning-only mTLS endpoint, never touched in operation; cannot be impersonated. |

**Rollout:** block the device's WAN → cold-reboot and confirm it reconnects and
keeps `/state` serving → run a multi-day soak, watching query logs. An occasional
boot-time probe to a blocked endpoint is benign; a tight retry storm would mean
something is actually needed (none was observed).

**Optional NTP insurance:** redirect `time.aws.com` to a local NTP source instead of
blocking, so the device always has valid time. Optional — the soak confirmed it works
without it.

## Backup & recovery

- **HA native backup (primary).** All state lives in `/data` and options ride along,
  so any HA backup including this add-on captures both. `config.yaml` sets
  `backup: cold`, so the Supervisor stops the add-on while `/data` is archived (never
  half-written). Recovery: restore the backup and the add-on returns with full
  history and calibration. Keep backups **off the Pi**.
- **No backup, cloud gone.** The device's identity is recoverable purely from what it
  announces on connect — run **`observe`** onboarding (no internet, no proxy); it
  fills `radarParams` and calibration from defaults, bringing **live data back
  immediately**. Only reading history and the `lstEmpty` baseline are lost, and both
  self-heal as new readings arrive (`lstEmpty` is also manually settable).

## Project layout

| Crate / dir           | Role                                                                                                                                                                      |
| --------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `aquilo-core`         | Pure, tested logic: raw-reading parser, state computer, pump-out detector, days-left estimator, battery curve, message builder. Time is injected for deterministic tests. |
| `aquilo-store`        | Persistence trait + atomic JSON-file impl (history, `lstEmpty`, last state, cached config).                                                                               |
| `aquilo-server`       | The binary: `serve` / `learn` / `observe` modes, MQTT client (`rumqttc`), topic templating, config.                                                                       |
| `addon/aquilo-server` | Home Assistant add-on packaging (Dockerfile, `config.yaml`, `build.yaml`, `DOCS.md`).                                                                                     |
| `capture/`            | Original Node reverse-engineering tooling, kept as the behavioral reference (see [Capture tooling](#capture-tooling)).                                                    |

`cargo test` covers core formulas against captured samples, persistence round-trips,
and an in-process-broker integration test exercising the full connect → handshake →
read round-trip without the physical device.

---

# Reference — reverse-engineering & protocol

Documents the protocol the server reimplements. Not needed for setup; here for
maintenance, debugging, and the curious.

## Hardware / architecture

- **Sensor node `ae5058`** — battery radar liquid-level sensor, outdoors. Measures
  distance to the liquid surface, transmits over radio. Identified as `node-4`.
- **Receiver `ae83fc`** — always-on indoor gateway at **`172.20.0.239`**
  (MAC `D8:BC:38:AE:83:FC`, ESP-based, firmware `1.7.1.9_sh_en`). Receives the radio
  data, talks to the cloud over MQTT, serves a local HTTP `/state`. Computes nothing
  — it displays whatever state the cloud sends.

All derived values (`pct`, `daysLeft`, `lvlToFull`, `lstEmpty`) are computed
server-side in the cloud. LAN (development): `172.20.0.0/24`; dev machine
`172.20.0.146`; device `172.20.0.239`.

## Cloud endpoints

| Domain                                                   | Resolves to                | Purpose                                | Notes                                                        |
| -------------------------------------------------------- | -------------------------- | -------------------------------------- | ------------------------------------------------------------ |
| `mqtt.aquilo.cloud`                                      | `57.128.198.238` (OVH VPS) | MQTT broker — telemetry up, state down | ports 1883 plain, 8883 TLS, 443. **Device uses plain 1883.** |
| `c2n0py5cened4k.credentials.iot.us-east-1.amazonaws.com` | AWS                        | AWS IoT credentials provider           | never touched in normal operation → **safe to block**        |
| `time.aws.com`                                           | AWS                        | NTP time sync                          | **NOT required** — see firewall test; no local NTP needed    |

The MQTT link is the **only** outbound dependency — exactly what this project stands
in for.

### Firewall test (2026-06-10)

With the transparent proxy running (device → laptop → real broker) and the firewall
blocking the device's WAN — cutting `time.aws.com` and the AWS IoT endpoint — the
device was **cold-rebooted** and: reconnected, completed the full handshake, received
all 3 retained messages, entered steady state (`PINGREQ`/`PINGRESP` every ~45 s), and
kept `/state` serving. → NTP and the AWS IoT provider are **not needed**, even on cold
boot. The only thing left to prove was that **our own broker** can stand in for the
proxy — which `serve` does.

## Local HTTP API (receiver, port 80)

Minimal embedded server; chokes on rapid back-to-back requests (one at a time,
`Connection: close`, ~1.5 s apart). Unknown paths → `404 Not found: /<path>`.

- `GET /` → text `Aquilo liquid level sensor`
- `GET /state` → cached state JSON (same content the cloud pushes over MQTT):

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

Connection: clientId `CieczSensor<rid>` (observed `CieczSensorae83fc`), user `<rid>`,
per-device numeric password, plaintext on `:1883`.

On connect the device publishes `online` + log lines, subscribes to 10 topics, and
the server delivers 3 **retained** messages it needs:

| Topic                                        | Retained payload                          |
| -------------------------------------------- | ----------------------------------------- |
| `/version/czujnik_szamba/ae83fc`             | `1.7.1.9_sh_en` (same fw → no OTA)        |
| `/users/ae83fc/state`                        | last known state JSON (above)             |
| `/users/ae83fc/receivers/ae83fc/radarParams` | `{"sensor":"ae5058","skip":9,"repeat":9}` |

Device → server topics:

- `/users/ae83fc/sensors/ae83fc/connection` → `online` / `offline`
- `/users/ae83fc/sensors/ae83fc/log` → version, MAC (firmware string is **learned
  from this** and echoed back, so no config needed for it)
- `/users/ae83fc/sensors/ae83fc/read` → **raw measurement** (below)

Device subscriptions: `/version/czujnik_szamba/ae83fc`, `/ping`,
`/users/ae83fc/state`, `/users/ae83fc/{restart,reset,cp,sensorCommand,updateSensor}`,
`/users/ae83fc/receivers/ae83fc/{params/set,radarParams}`.

Keepalive: device sends `PINGREQ` ~every 45 s; server also publishes app-level
`/ping` (~20 min). With proper state replies + the 3 retained messages the device
stays connected for hours; a silent broker makes it cycle offline→reconnect every
~40 min — so correct replies are essential. `params/set` is never required.

### Measurement round-trip (the core exchange)

Device publishes to `/users/ae83fc/sensors/ae83fc/read`:

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

Server replies ~90 ms later on retained `/users/ae83fc/state` with the computed
state. `salt` is an anti-replay nonce, not echoed back.

## Derived-value formulas (reverse-engineered)

`lvl` = radar distance to the liquid surface, cm. Smaller distance = fuller tank.

- `lvl = read1 / 100` (15020 → 150.2; the `raw` field `1502` is mm)
- `lvlToFull = lvl - FULL_DIST`, with `FULL_DIST ≈ 40` cm
- `pct = round((EMPTY_DIST - lvl) / (EMPTY_DIST - FULL_DIST) * 100)`, `EMPTY_DIST ≈ 178` cm
- `daysLeft` = fill-rate projection since `lstEmpty`
- `lstEmpty` = timestamp of last pump-out (detected as a large level drop)
- `bat` = battery mV → % curve (`3770 mV → 83%`; one data point so far)

`FULL_DIST` / `EMPTY_DIST` are server-side tank config; offline they're our own
calibration constants (the `full_dist` / `empty_dist` add-on options). `daysLeft` and
`bat%` are reasonable approximations, not exact replicas of the vendor algorithm.
Samples: `(lvl 152.8 → pct 18)`, `(lvl 150.4 → pct 20, lvlToFull 110)`,
`(lvl 150.2 → pct 20, lvlToFull 110, daysLeft 51)`.

## How it was reverse-engineered

1. Port-scanned the receiver → found local HTTP on :80, then `/state`.
2. Found cloud domains via AdGuard query log + router logs.
3. Identified `mqtt.aquilo.cloud` (OVH) by grabbing its TLS cert; confirmed open
   ports incl. plaintext 1883.
4. Redirected `mqtt.aquilo.cloud → 172.20.0.146` via an AdGuard DNS rewrite.
5. Ran a logging broker (`capture/broker.js`) → captured CONNECT, credentials,
   subscriptions, publishes.
6. Ran a transparent proxy (`capture/proxy.js`, device ↔ real broker, both directions
   decoded) → captured the server's retained messages and a full measurement
   round-trip.

### Capture tooling

The Node tooling in `capture/` is kept as diagnostics and as the behavioral reference
for the Rust `learn` mode and the protocol integration test:

- `broker.js` — aedes broker that accepts the device and logs everything (no upstream).
- `proxy.js` — transparent MQTT proxy to the real broker (upstream by IP to avoid the
  DNS loop), decodes both directions. `UPSTREAM_HOST/PORT`, `LISTEN_PORT` env-overridable.
- `capture.py` + `gen-cert.sh` — self-signed TLS logger to test whether the AWS
  endpoints validate the server cert. Likely unneeded (device uses plaintext MQTT).
- `README.md` — run instructions.

## Open data gaps (non-blocking)

Accuracy improvements only: exact `/ping` cadence; more `(lvl, pct, lvlToFull)`
samples to pin `FULL_DIST` / `EMPTY_DIST`; more battery `mV → %` points.
