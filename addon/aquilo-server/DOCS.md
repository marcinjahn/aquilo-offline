# Aquilo offline server

This add-on replaces the Aquilo vendor cloud on your own network. It connects to
your **Mosquitto** broker as a client, keeps the receiver's MQTT session alive,
and on every raw reading republishes the computed retained `/state` (tank %, level,
battery, days-left, last-read). The receiver caches that and serves it on its own
HTTP `/state`, exactly as it did with the cloud — so nothing on the device changes.

The add-on creates **no Home Assistant entities**. You surface the data with a
built-in RESTful sensor pointed at the device's `/state` (see below).

## Prerequisites

1. The official **Mosquitto broker** add-on, installed and started.
2. Your network redirects `mqtt.aquilo.cloud` to your Home Assistant host (e.g. an
   AdGuard/router DNS rewrite) so the device connects to Mosquitto instead of the
   cloud. The device speaks plaintext MQTT on port `1883`.
3. Your device identity (`receiver_id`, `sensor_id`, MQTT user/pass). If you don't
   know these, recover them with the one-time `learn` (proxy through the real
   cloud) or `observe` (cloud-free) onboarding, which you run from a computer on
   your network rather than this add-on — see the project README. Then copy the
   recovered values into the options below.

## Step 1 — Add the device's MQTT login to Mosquitto

The receiver authenticates to Mosquitto with the fixed credentials baked into its
firmware (the `mqtt_user` / `mqtt_pass` you enter in the options below). Mosquitto
must accept that login. In the **Mosquitto broker** add-on configuration, add a
local user:

```yaml
logins:
  - username: ae83fc # your mqtt_user (usually the receiver_id)
    password: "48007129" # your mqtt_pass
```

Save and restart the Mosquitto add-on. (This add-on connects with its own,
Home-Assistant-managed MQTT service account — you do **not** need to add a login
for it.)

## Step 2 — Configure and start this add-on

Fill in the options:

| Option                        | Meaning                                                        |
| ----------------------------- | -------------------------------------------------------------- |
| `receiver_id`                 | Receiver/gateway id (`<rid>`); all MQTT topics derive from it. |
| `sensor_id`                   | Radio sensor-node id reported in `/state`.                     |
| `sensor_name`                 | Display name for the sensor.                                   |
| `mqtt_user` / `mqtt_pass`     | The device's fixed MQTT credentials (also added to Mosquitto). |
| `firmware_version`            | Echoed back so the device never attempts an OTA update.        |
| `full_dist` / `empty_dist`    | Tank calibration (cm), driving `pct` and `lvlToFull`.          |
| `radar_skip` / `radar_repeat` | Radar/radio params published to the sensor node.               |
| `ping_interval_secs`          | App-level `/ping` cadence (~20 min default).                   |
| `history_max_len`             | Rolling reading-history cap (drives `daysLeft`).               |
| `pump_out_drop_pct`           | Fullness drop (pct points) counted as a pump-out (`lstEmpty`). |
| `log_level`                   | Add-on log verbosity.                                          |

Options take effect on **restart**. State (history, `lstEmpty`, the last computed
`/state`) is stored in `/data` and persists across restarts, host reboots, and
add-on updates.

Start the add-on and watch the log: you should see it connect to Mosquitto, seed
the retained messages, and — once the device reconnects — log incoming readings
and the recomputed `/state`. The device's HTTP `/state` then serves live data.

## Step 3 — Surface the data in Home Assistant (RESTful sensor)

The receiver's embedded HTTP server is minimal and chokes on rapid requests, so
poll **gently**: one request at a time, `Connection: close`, ~60 s apart (readings
only arrive a few times a day, so this loses nothing). Add to your
`configuration.yaml`, replacing the IP with your receiver's:

```yaml
rest:
  - resource: "http://172.20.0.239/state"
    scan_interval: 60
    timeout: 10
    headers:
      Connection: close
    sensor:
      - name: "Aquilo tank level"
        unique_id: aquilo_tank_pct
        value_template: "{{ value_json.sensors[0].pct }}"
        unit_of_measurement: "%"
        state_class: measurement
      - name: "Aquilo raw level"
        unique_id: aquilo_tank_lvl
        value_template: "{{ value_json.sensors[0].lvl }}"
        unit_of_measurement: "cm"
        state_class: measurement
      - name: "Aquilo battery"
        unique_id: aquilo_battery
        value_template: "{{ value_json.sensors[0].bat }}"
        unit_of_measurement: "%"
        device_class: battery
        state_class: measurement
      - name: "Aquilo days left"
        unique_id: aquilo_days_left
        value_template: "{{ value_json.sensors[0].daysLeft }}"
        unit_of_measurement: "d"
      - name: "Aquilo last read"
        unique_id: aquilo_last_read
        value_template: "{{ value_json.sensors[0].lstRead }}"
        device_class: timestamp
      - name: "Aquilo last emptied"
        unique_id: aquilo_last_empty
        value_template: "{{ value_json.sensors[0].lstEmpty }}"
        device_class: timestamp
```

A single `rest:` block with multiple `sensor:` entries issues **one** request per
`scan_interval` and fans the JSON out into all the entities, which is exactly the
gentle polling the device needs. Restart Home Assistant (or reload RESTful
entities) to pick it up.

## Backup & recovery

State lives in `/data` and the options are part of the add-on, so any Home
Assistant backup that includes this add-on captures both. `backup: cold` stops the
add-on while `/data` is archived, guaranteeing a consistent snapshot. To recover
after a host failure, install a fresh Home Assistant, restore the backup, and the
add-on returns with its full history and calibration — no separate export step.

Keep backups off the Pi, and include this add-on in any scheduled backups.

If you have **no backup and no cloud**, you can still reconstruct a working config:
the device's identity is recoverable from what it announces on connect. Run the
`observe` onboarding (no internet needed) to regenerate the IDs/credentials, then
re-enter them here. Only the reading history and the `lstEmpty` baseline are lost,
and both self-heal as new readings arrive.
