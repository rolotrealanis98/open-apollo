# The install report

What `scripts/install.sh` sends to `POST https://open-apollo-api.rolotrealanis.workers.dev/reports`
when, and only when, the user ticks the wizard's report box or passes `--send-report`. The
same JSON stays in `/tmp/open-apollo-install-report.json` either way. Every string in it has
the user name, home folder, host name, machine id, serial numbers and MAC addresses taken out
(`scripts/oa_redact.py`). This page is the contract for the dashboard's worker.

## Top level (version 2)

| Field | Type | What |
|---|---|---|
| `version` | int | `2` |
| `script_version` | string | install.sh's version |
| `timestamp` | string | UTC, `2026-10-19T15:04:05Z` |
| `success` | bool | the driver built and its dependencies were there |
| `source` | string | `user`, `test` or `ci` |
| `type` | string | `thunderbolt` (install-usb.sh sends `usb`) |
| `system` | object | `distro`, `kernel`, `arch`, `ram_mb`, `cpu`, `secure_boot`, `iommu` |
| `audio` | object | `pipewire_version`, `wireplumber_version`, `pulseaudio_running`, `existing_cards`, `snd_modules` |
| `audio_state` | object | `aplay`, `arecord`, `pw_dump` (the Apollo's PipeWire objects only, other audio objects counted by class) |
| `apollo` | object | `detected`, `pcie_device`, `thunderbolt_controller` |
| `iommu_groups` | array | `{group, device, description}` |
| `steps` | object | per step (`distro`, `deps`, `build`, `dkms`, `configs`, `init`, `daemon`, `plugins`): `{status, detail?}` |
| `logs` | object | the driver's, Thunderbolt's and the IOMMU's kernel log lines |
| `github_username` | string | only when typed in at the terminal prompt |
| `plugins` | object | see below; only in a report that is sent, and not with `--no-plugin-report` |

## `plugins` (schema `open-apollo-plugin-report/1`)

The plug-ins the user's Apollo itself says are authorized, so the project can verify those
first. Made by `mixer-engine/plugin_report.py` from one read-only board query
(`UA_IOCTL_BOARD_QUERY`, the answer the daemon mirrors on `/plugins/N/Authorized`), and
re-checked by `plugin_report.sanitize()` just before sending, which drops anything else.

With an answer:

```json
"plugins": {
  "schema": "open-apollo-plugin-report/1",
  "available": true,
  "authorized": [2100, 2276, 2500],
  "native": {
    "built_mono": [2276],
    "built_stereo": [2276],
    "failed": [2100]
  }
}
```

- `authorized`: sorted, unique render ids (UA's plug-in product numbers, 2048..2815) whose
  board-table word means authorized. Demo states (running, not started, expired) and
  unavailable plug-ins are not listed and not counted.
- `native.built_mono`, `native.built_stereo`: those of `authorized` whose native-hosting
  bundle `scripts/setup-plugin-hosting.sh` built and passed offline on this machine, per
  layout. Empty without plug-in hosting.
- `native.failed`: those of `authorized` the plug-in mapper tried on this machine and could
  not build (in neither layout). A plug-in that is not on the known-good list is in none of
  the three.
- Every `native` id is also in `authorized`.

Without an answer:

```json
"plugins": {"schema": "open-apollo-plugin-report/1", "available": false, "reason": "plugin_host_off"}
```

`reason` is one of `no_device` (no driver or no Apollo), `plugin_host_off` (the driver runs
without `plugin_host=1`: the Apollo only answers with plug-in hosting on), `query_failed`,
`bad_answer`.

Never in it: licence states other than authorized, demo days, serial numbers, UA account
details, device or host ids, plug-in names, file paths, error text.

## What the worker should do with it

The worker stores the whole body in `raw_json` already, so older workers keep working. The
dashboard worker (`open-apollo-dashboard`, `src/worker.js`) should, for `plugins`:

1. validate it as above (drop anything that isn't a list of ints in 2048..2815, drop native
   ids that aren't authorized) and store it in its own column, `plugins_json`;
2. keep it out of the public per-report API (`/api/reports/:id` shows it to the admin token
   only), like the logs;
3. publish only counts: `GET /api/plugins` → per render id, how many reports list it as
   authorized, built (mono, stereo) and failed, so verification can follow what people own.
