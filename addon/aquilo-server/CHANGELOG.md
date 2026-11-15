# Changelog

## 0.1.3

- The add-on no longer gives up when Mosquitto is running but doesn't advertise
  its MQTT service, which can happen after a backup restarts both add-ons. It now
  waits up to two minutes for the service, then falls back to the Mosquitto
  credentials it last received, instead of exiting and staying stopped until
  restarted by hand.

## 0.1.2

- `daysLeft` is now projected from the average fill rate since the last
  pump-out, instead of a fit over the whole rolling history. The old fit mixed
  readings from before and after a pump-out, which flattened the trend into
  absurd values (thousands of days). Right after a pump-out, the previous cycle's
  fill rate is reused until the new cycle is about three days old.
- A pump-out is confirmed only when the next reading stays low. A single low
  reading (a radar glitch) no longer resets `lstEmpty` and is dropped from the
  history.
- The last reading, which the broker re-delivers on every reconnect, is no
  longer counted again as a new reading.

## 0.1.1

- `/state` now starts as an all-zero placeholder (`lvl`, `pct`, `bat`,
  `lvlToFull`, `daysLeft` = `0`, `lstEmpty` blank) until the first real reading
  arrives, instead of vendor-sample numbers that looked like live data.
- Documented cold-start behavior in DOCS ("First start — what to expect"): the
  placeholder values, when readings actually arrive, and why `daysLeft` and
  `lstEmpty` start empty and self-heal.

## 0.1.0

- Initial release. Local stand-in for the Aquilo vendor cloud: connects to
  Mosquitto as a client, keeps the receiver's session alive, and republishes the
  computed retained `/state` so the device's own HTTP `/state` keeps working with
  the internet cut.
