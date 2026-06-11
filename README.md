# Aquilo Water Tank Sensor Offline Server

Goal: run an Aquilo septic-tank liquid-level sensor fully offline (no internet /
no vendor cloud), with local access to measurements, integrated into Home
Assistant.
The solution has been tested with the WiFI variant of the sensor.

## Hardware / architecture

Two devices:

- **Sensor node `ae5058`** — battery radar liquid-level sensor, outdoors. Measures
  distance to the liquid surface, transmits over radio. Identified as `node-4`.
- **Receiver `ae83fc`** — always-on indoor gateway at **`172.20.0.239`**
  (MAC `D8:BC:38:AE:83:FC`, ESP-based, firmware `1.7.1.9_sh_en`). Receives the
  radio data, talks to the cloud over MQTT, and serves a local HTTP `/state`.
  It does **not** compute anything — it displays whatever state the cloud sends.

All derived values (`pct`, `daysLeft`, `lvlToFull`, `lstEmpty`) are computed
server-side in the cloud. The receiver is a dumb pipe.

LAN: `172.20.0.0/24`; dev machine `172.20.0.146`; device `172.20.0.239`.

## Cloud endpoints

| Domain                                                   | Resolves to                                           | Purpose                                         | Notes                                                                      |
| -------------------------------------------------------- | ----------------------------------------------------- | ----------------------------------------------- | -------------------------------------------------------------------------- |
| `mqtt.aquilo.cloud`                                      | `57.128.198.238` (OVH VPS `vps-928313f2.vps.ovh.net`) | MQTT broker — telemetry up, computed state down | ports 1883 plain, 8883 TLS (LE cert), 443. **Device uses plaintext 1883.** |
| `c2n0py5cened4k.credentials.iot.us-east-1.amazonaws.com` | AWS                                                   | AWS IoT credentials provider                    | never touched during normal operation → **confirmed safe to block**        |
| `time.aws.com`                                           | AWS                                                   | NTP time sync                                   | **NOT required** — see firewall test below; no local NTP needed            |

The device is not autonomous: it needs a live MQTT link to serve `/state`
(blocking _all_ internet, MQTT included, makes `/state` stop responding). But
the MQTT link is the **only** outbound dependency — see the firewall test below.

### Firewall test (2026-06-10) — AWS/NTP independence confirmed

With the transparent proxy running (device → laptop → real broker, so the MQTT
path stays alive) and the router firewall blocking the device's WAN — cutting
`time.aws.com` and the AWS IoT endpoint — the device was **cold-rebooted** and:

- reconnected, completed the full handshake, received all 3 retained messages,
- entered steady state (`PINGREQ`/`PINGRESP` every ~45 s),
- kept `/state` serving fresh data.

→ `time.aws.com` (NTP) and the AWS IoT credentials provider are **not needed**,
not even on cold boot. No local NTP required. The only thing left to prove for
full offline operation is that **our own broker** (no cloud upstream) can stand
in for the proxy. (Longer-run stability with the firewall on is still being
observed.)

## Local HTTP API (on the receiver, port 80)

Minimal embedded server; chokes on rapid back-to-back requests (query one at a
time, `Connection: close`, ~1.5s apart). Unknown paths → `404 Not found: /<path>`.

- `GET /` → text `Aquilo liquid level sensor`
- `GET /state` → the cached state JSON (same content the cloud pushes over MQTT):

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

Connection: clientId `CieczSensorae83fc`, user `ae83fc`, pass `48007129`,
plaintext on `:1883`.

On connect the device publishes `online` + log lines, subscribes to 10 topics,
and the server delivers 3 **retained** messages it needs:

| Topic                                        | Retained payload                          |
| -------------------------------------------- | ----------------------------------------- |
| `/version/czujnik_szamba/ae83fc`             | `1.7.1.9_sh_en` (same fw → no OTA)        |
| `/users/ae83fc/state`                        | last known state JSON (above)             |
| `/users/ae83fc/receivers/ae83fc/radarParams` | `{"sensor":"ae5058","skip":9,"repeat":9}` |

Device → server topics:

- `/users/ae83fc/sensors/ae83fc/connection` → `online` / `offline`
- `/users/ae83fc/sensors/ae83fc/log` → version, MAC
- `/users/ae83fc/sensors/ae83fc/read` → **raw measurement** (see below)

Device subscriptions (commands/config it listens for): `/version/czujnik_szamba/ae83fc`,
`/ping`, `/users/ae83fc/state`, and `/users/ae83fc/{restart,reset,cp,sensorCommand,updateSensor}`,
`/users/ae83fc/receivers/ae83fc/{params/set,radarParams}`.

Keepalive: device sends MQTT `PINGREQ` ~every 45s; server also publishes an
app-level `/ping` periodically. With proper state replies + the 3 retained
messages the device stays connected for hours; a silent broker (no replies)
makes it cycle offline→reconnect every ~40 min.

`params/set` is never required — the device operated without it.

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

`lvl` = radar distance to the liquid surface, in cm. Smaller distance = fuller tank.

- `lvl = read1 / 100` (15020 → 150.2; the `raw` field `1502` is mm)
- `lvlToFull = lvl - FULL_DIST`, with `FULL_DIST ≈ 40` cm
- `pct = round((EMPTY_DIST - lvl) / (EMPTY_DIST - FULL_DIST) * 100)`, with `EMPTY_DIST ≈ 178` cm
- `daysLeft` = fill-rate projection since `lstEmpty`
- `lstEmpty` = timestamp of last pump-out (detected as a large level drop); currently `2026-05-30`
- `bat` = battery mV → % curve (`3770 mV → 83%`; only one data point so far)

`FULL_DIST` / `EMPTY_DIST` are server-side tank config; offline they become our
own calibration constants. `daysLeft` and `bat%` will be approximations.

Samples used: `(lvl 152.8 → pct 18)`, `(lvl 150.4 → pct 20, lvlToFull 110)`,
`(lvl 150.2 → pct 20, lvlToFull 110, daysLeft 51)`.

## What we did

1. Port-scanned the receiver → found local HTTP on :80, then `/state`.
2. Found cloud domains via AdGuard query log + router logs.
3. Identified `mqtt.aquilo.cloud` (OVH) by grabbing its TLS cert; confirmed open
   ports incl. plaintext 1883.
4. Redirected `mqtt.aquilo.cloud → 172.20.0.146` via an AdGuard DNS rewrite.
5. Ran a logging MQTT broker (`capture/broker.js`) → captured the device's
   CONNECT, credentials, subscriptions, and publishes.
6. Ran a transparent MQTT proxy (`capture/proxy.js`, device ↔ real broker, both
   directions decoded) → captured the server's retained messages and a full
   measurement round-trip (raw read → computed state).

### Capture tooling (`capture/`)

- `broker.js` — aedes broker that accepts the device and logs everything (no upstream).
- `proxy.js` — transparent MQTT proxy to the real broker (upstream by IP to avoid
  the DNS loop), decodes both directions. `UPSTREAM_HOST/PORT`, `LISTEN_PORT` env-overridable.
- `capture.py` + `gen-cert.sh` — self-signed TLS logger to test whether the AWS
  endpoints validate the server cert. Likely unneeded (device uses plaintext MQTT).
- `README.md` — run instructions.

## Offline plan

Local always-on MQTT broker (device reaches it via the permanent AdGuard rewrite)
that:

1. Serves the 3 retained connect-time messages (`/version` = same fw, last
   `/state`, `radarParams`).
2. Publishes `/ping` periodically.
3. On each `/read`: parses the raw reading, computes the state, publishes retained
   `/users/ae83fc/state`. Keeps local history for `daysLeft` / `lstEmpty`.
4. Exposes configurable `FULL_DIST` / `EMPTY_DIST` calibration.

Then: permanently rewrite `mqtt.aquilo.cloud` → the server host, block the
device's other internet (no local NTP needed — confirmed above), and feed
values into Home Assistant (via the device's `/state` or directly from our broker).

### Pending decisions

- Deployment host (always-on; HA box / Pi / NAS / Docker).
- Server language (recommended: Node + aedes, since the broker is the hard part).
- Fidelity (full derived-value replica vs. just raw level + battery).

### Open data gaps (non-blocking)

- Exact `/ping` cadence.
- More `(lvl, pct, lvlToFull)` samples to pin `FULL_DIST` / `EMPTY_DIST`.
- More battery `mV → %` points.
