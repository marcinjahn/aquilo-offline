# Changelog

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
