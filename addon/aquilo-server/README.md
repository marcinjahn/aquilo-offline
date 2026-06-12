# Aquilo offline server (Home Assistant add-on)

Runs the [`aquilo-server`](../../README.md) binary in `serve` mode as a client of
your **Mosquitto** broker, standing in for the Aquilo vendor cloud. The receiver
keeps serving its HTTP `/state` with the internet cut; surface it in Home
Assistant with a built-in RESTful sensor.

See **[DOCS.md](DOCS.md)** for setup: adding the device's MQTT login to Mosquitto,
the add-on options, the RESTful-sensor config, and backup/recovery.

## Install

1. Settings → Add-ons → Add-on store → ⋮ → **Repositories**, and add this repo's
   URL.
2. Install **Aquilo offline server**, set the options (DOCS.md), and start it.

> Images are pulled from GHCR (`ghcr.io/marcinjahn/<arch>-aquilo-server`), published
> by the `addon.yml` workflow. If you fork this repo, replace `marcinjahn` in
> `config.yaml`, `build.yaml`, and `repository.yaml` with your own GitHub owner.
