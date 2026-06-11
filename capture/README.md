# Aquilo traffic capture

Goal: find out how the sensor (`172.20.0.239`) talks to its backend so we can
run it fully offline. The real backend is **`mqtt.aquilo.cloud`** (`57.128.198.238`,
an OVH VPS). Blocking the device's internet kills its local `/state` endpoint, so
the device depends on this MQTT link even to serve local data.

| Domain                                                   | Purpose (likely)                                        | Ports                                   |
| -------------------------------------------------------- | ------------------------------------------------------- | --------------------------------------- |
| `mqtt.aquilo.cloud`                                      | vendor MQTT broker — telemetry up + computed state down | **1883 plain**, 8883 TLS (LE cert), 443 |
| `c2n0py5cened4k.credentials.iot.us-east-1.amazonaws.com` | AWS IoT credentials provider (secondary)                | 443                                     |
| `time.aws.com`                                           | NTP — leave pointed at the real internet                | 123/udp                                 |

Confirmed: the device talks **plaintext MQTT on :1883** (clientId `CieczSensorae83fc`,
user `ae83fc`, pass `48007129`). No TLS to defeat.

### Step 1 — transparent proxy (primary capture)

`proxy.js` forwards device ↔ real broker (`57.128.198.238:1883`, by IP to avoid
the DNS-rewrite loop) and decodes **both directions**, so we capture what the
server sends back (`/users/ae83fc/state` and friends) — the payloads we must
reimplement. Keep the AdGuard rewrite in place; just run:

```sh
npm install            # once
node capture/proxy.js
```

DNS rewrite (keep it): `mqtt.aquilo.cloud -> 172.20.0.146`. Leave `time.aws.com`
alone. Power-cycle the sensor (or wait ~40 min) to force a reconnect, then read
the log: `C->S` = device→server, `S->C` = server→device.

What we need from this: the server→device PUBLISH payloads on `/users/ae83fc/state`,
`/users/ae83fc/receivers/ae83fc/params/set`, `/users/ae83fc/receivers/ae83fc/radarParams`,
`/ping`, and `/version/czujnik_szamba/ae83fc`, plus any measurement topics the
device publishes once the handshake completes.

### Alt — silent logging broker

`broker.js` (aedes) just accepts the device and logs CONNECT/SUB/PUBLISH without
an upstream. Useful to confirm what the device sends, but it can't elicit the
server's replies (device cycles every ~40 min waiting for a handshake). Use the
proxy instead for protocol capture.

## Capture step (AWS endpoints) — only if needed

`capture.py` + `gen-cert.sh` impersonate the AWS `credentials.iot` / ELB
endpoints (self-signed cert) to test whether the device validates those certs.
Needs sudo for :443:

```sh
bash capture/gen-cert.sh        # once
! sudo python3 capture/capture.py --ports 443,8883
```

Then rewrite the AWS domain to `172.20.0.146` in AdGuard. Handshake completes =
no validation (impersonable); handshake fails = validates the cert chain.

## Cleanup

Remove the AdGuard DNS rewrite(s) to restore normal operation; Ctrl-C the broker.
