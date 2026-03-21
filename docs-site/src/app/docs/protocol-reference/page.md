---
title: Protocol Reference
---

The mixer daemon serves two TCP protocol endpoints and an optional WebSocket endpoint. All three provide access to the same underlying state tree, but use different wire formats.

---

## Protocol overview

| Endpoint | Port | Wire format | Used by |
|---|---|---|---|
| Mixer Engine | TCP:4710 | Null-terminated JSON text | ConsoleLink (iOS/iPad) |
| Mixer Helper | TCP:4720 | Text commands in, UBJSON binary out | UA Console (desktop) |
| WebSocket | WS:4721 | JSON text frames | UA Connect (Electron) |

All protocols share the same command verbs: `get`, `set`, `subscribe`, `unsubscribe`, and `post`.

---

## TCP:4710 — Mixer Engine protocol

### Message framing

Messages are UTF-8 text terminated by a null byte (`\x00`). Multiple messages can arrive in a single TCP segment.

```
get /devices/0/DeviceOnline/value\0
set /devices/0/outputs/18/Mute/value true\0
```

### Command format

```
<verb> <path>[?<query_params>] [<value>]\0
```

**Verbs:**

| Verb | Description | Has value? |
|---|---|---|
| `get` | Read a value or subtree | No |
| `set` | Write a value | Yes |
| `subscribe` | Register for change notifications | No |
| `unsubscribe` | Remove change notification | No |
| `post` | Action request (e.g., challenge-response) | Optional |

### Query parameters

Parameters are appended to the path after `?`:

- `recursive=1` — Include all children in the response
- `propfilter=value,type` — Only include named properties

### Response format

Responses are JSON objects, also null-terminated:

```json
{"path":"/devices/0/DeviceOnline/value","data":true}\0
```

Error responses:

```json
{"path":"/devices/0/nonexistent","error":"Unable to resolve path for get."}\0
```

### Example session

```
→ get /devices/0/DeviceOnline/value\0
← {"path":"/devices/0/DeviceOnline/value","data":true}\0

→ subscribe /devices/0/outputs/18/Mute/value\0
(no immediate response — notifications arrive on change)

→ set /devices/0/outputs/18/Mute/value true\0
← {"path":"/devices/0/outputs/18/Mute/value","data":true}\0

→ get /?recursive=1&propfilter=value,type\0
← {"path":"/","data":{...entire tree...}}\0
```

### Client identification

On connect, clients may send a JSON identification message:

```json
{"path":"networkID","data":"abc123def456"}\0
```

The daemon generates unique network and process IDs (hex UUIDs) and echoes them on request.

---

## TCP:4720 — Mixer Helper protocol

### Connection handshake

The 4720 protocol begins in text mode and transitions to UBJSON binary after a handshake:

1. Client connects
2. Client sends: `post command_format?func_id=1 2\0`
3. Server responds with JSON confirming format 2
4. All subsequent responses are UBJSON-framed binary

### Command format (text)

Commands use the same verb/path structure as 4710, with additional query parameters:

```
get /devices?message_id=abc:1&recursive=1
set /devices/0/inputs/0/Gain/value?message_id=abc:2&cmd_id=set_command 10.0
subscribe /devices/0/inputs/0/Gain/value?message_id=abc:3
```

**Additional parameters:**

| Parameter | Description |
|---|---|
| `message_id` | Echoed in response for request/response matching |
| `cmd_id` | Command identifier (e.g., `set_command`) |
| `flatvalue` | `1` for flat value format |
| `func_id` | Function identifier for `post` commands |

### UBJSON response framing

After the `command_format 2` handshake, responses use UBJSON binary framing:

```
b"UBJS" + uint32_le(payload_length) + ubjson_payload + b"\x00\x00"
```

The UBJSON payload contains the same `{path, data, parameters}` structure as JSON responses, but binary-encoded.

**UBJSON type markers:**

| Marker | Type | Encoding |
|---|---|---|
| `Z` | null | (no data) |
| `T` | true | (no data) |
| `F` | false | (no data) |
| `i` | int8 | 1 byte signed |
| `U` | uint8 | 1 byte unsigned |
| `I` | int16 | 2 bytes little-endian |
| `l` | int32 | 4 bytes little-endian |
| `L` | int64 | 8 bytes little-endian |
| `D` | float64 | 8 bytes big-endian IEEE 754 |
| `S` | string | length-prefixed UTF-8 |
| `{` | object | counted: `{#<count> key1 val1 ...` |
| `[` | array | counted: `[#<count> val1 val2 ...` |

Note: UA's UBJSON implementation uses **little-endian** for multi-byte integers, which differs from the UBJSON specification (big-endian). Float64 values remain big-endian per IEEE 754.

Object keys are encoded as `<length><utf8_bytes>` without an `S` type prefix.

### Response with message_id

When a request includes `message_id`, the response echoes it:

```json
{"path":"/devices/0/Gain/value","data":10.0,"parameters":{"message_id":"abc:1"}}
```

---

## WS:4721 — WebSocket protocol

The WebSocket endpoint uses JSON text frames. Command and response format matches TCP:4720 (text verb/path commands, JSON responses), but without the UBJSON binary layer.

```
→ get /devices?message_id=ws:1&recursive=1
← {"path":"/devices","data":{...},"parameters":{"message_id":"ws:1"}}
```

---

## Subscription and notifications

### Subscribing to changes

```
subscribe /devices/0/outputs/18/Mute/value\0
```

After subscribing, the daemon pushes a notification whenever the value changes:

```json
{"path":"/devices/0/outputs/18/Mute/value","data":true}\0
```

Notifications are triggered by:

- Another client setting a value
- Hardware readback detecting a physical control change (knob, button)
- Internal state changes (initialization, metering)

### Batch subscribe (4710)

ConsoleLink sends batch subscription requests:

```
subscribe {"paths":["/devices/0/outputs/18/Mute/value","/devices/0/outputs/18/DimOn/value"]}\0
```

---

## Metering data

### Meter paths

Audio meters are exposed at predictable state tree paths:

```
/devices/0/inputs/<ch>/meters/0/MeterLevel/value     — current level (dBFS)
/devices/0/inputs/<ch>/meters/0/MeterPeakLevel/value  — peak hold level (dBFS)
/devices/0/inputs/<ch>/meters/0/MeterClip/value       — clip indicator (boolean)
```

Output meters follow the same pattern under `/devices/0/outputs/<ch>/meters/`.

### MeterPulse heartbeat

The daemon sends `/MeterPulse/value` at ~10 Hz as an incrementing counter. Clients use this as a heartbeat to confirm the daemon is alive and metering is active.

### Meter values

| Property | Type | Range | Description |
|---|---|---|---|
| `MeterLevel` | float | -77.0 to 0.0 | Current RMS level in dBFS |
| `MeterPeakLevel` | float | -77.0 to 0.0 | Peak hold (2s hold, 20 dB/s decay) |
| `MeterClip` | bool | true/false | Clip indicator (2s hold) |

Silence floor is -77.0 dBFS. Meters are computed in software from ALSA PCM capture samples.

---

## State tree paths

The state tree is hierarchical. Key paths for an Apollo x4:

| Path | Type | Description |
|---|---|---|
| `/initialized` | bool | Daemon initialization complete |
| `/SampleRate` | int | Current sample rate (Hz) |
| `/DeviceConnectionMode` | string | `"TB"` (Thunderbolt) |
| `/devices/0/DeviceOnline` | bool | Device connected |
| `/devices/0/inputs/<n>/preamps/0/Gain` | float | Preamp gain (dB) |
| `/devices/0/inputs/<n>/preamps/0/48V` | bool | Phantom power |
| `/devices/0/outputs/18/Mute` | bool | Monitor mute |
| `/devices/0/outputs/18/CRMonitorLevelTapered` | float | Monitor volume (0.0-1.0) |
| `/TotalDSPLoad` | float | DSP load percentage |

---

## Further reading

- [Daemon Setup](/docs/daemon-setup) — running the daemon
- [Daemon Configuration](/docs/daemon-configuration) — all command-line flags
- [Architecture Overview](/docs/architecture-overview) — system diagram
