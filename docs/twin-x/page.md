---
title: Apollo Twin X
---

# Apollo Twin X

The Apollo Twin X DUO uses device type `0x23` and PCI subsystem ID `0x0019`.
The subsystem ID is required because serial prefix `2024` also identifies an
Apollo Solo in the current serial table.

## Linux support status

The Twin X DUO is partially verified on Linux with 10 playback channels, 16
capture channels, and one Hi-Z input. PipeWire playback and capture work at 48
kHz. Discord voice calls also work with the Apollo as the input and output.

The following areas remain unverified:

- Twin X routing tables and DSP program blocks
- Preamp PAD, 48 V, and mic or line relays
- Apollo Twin X QUAD and Apollo Twin X Gen 2

## Safe first contact

Use `probe_only=1` to identify the device without starting interrupts, direct
memory access, or ALSA:

```bash
cd driver
make
sudo insmod ./ua_apollo.ko probe_only=1
sudo dmesg | tail -50
sudo rmmod ua_apollo
```

After a full initialization, removing `ua_apollo` is the documented brick path
on the Apollo x4. The Thunderbolt link can drop and require a cold boot.

On the tested Twin X DUO, five consecutive `probe_only=1` load and unload
cycles completed without a link loss. Two full initialization unloads also
completed without a link loss. The PCI Express endpoint remained present at
`0000:3e:00.0` after the final unload, and the device did not need a cold boot.

## Measured Linux results

Testing used an Apollo Twin X DUO over Thunderbolt 3 on an Arch Linux host with
kernel 7.1.5. The driver reported device type `0x23`, subsystem ID `0x0019`,
firmware version 2, and two SHARC DSPs.

The ring buffer transport clocked after both warm and cold starts. PipeWire
exposed 10 playback channels and 16 capture channels at 48 kHz. The tests found
no kernel oops, call trace, or uncorrectable PCI Express error.

No routing configuration exists for device type `0x23`. Applications that can
use raw multichannel input and output work, but Universal Audio channel labels
and routing remain incomplete.

## Device description source

### Where UA keeps the device description

The question that produced this document: *how would an Apollo engineer build this?*
Answer: they would not hand-author routing blobs. They would read a device
description. That description ships with the UA software and can be read off
disk — **no SIP change, no DTrace, no kernel tracing.**

### Location

```
/Library/Application Support/Universal Audio/Apollo/UA Mixer Engine.app/Contents/MacOS/UA Mixer Engine
```

The Mixer Engine binary embeds **52 XML documents** — default Console session
templates, one per Apollo model/variant. Each begins `<?xml version='1.0'` and
contains a `kRoot` object. The full set appears twice in the binary (around
offsets ~18.3M and ~52.6M); the two copies are identical in content.

Object types present: `kRoot`, `kMixer`, `kInput`, `kOutput`, `kAuxBus`,
`kEffect`. Counts across all documents: 1168 `kInput`, 858 `kOutput`,
96 `kAuxBus` (i.e. 2 per device, matching the manual's "two independent stereo
Auxiliary buses"), 50 each of `kRoot`/`kMixer`.

### Schema

Every channel is an ordered object carrying a name and a type:

```xml
<mixer_object type='kInput' relative_index='2' version='2'>
  <property id='kPropName'   type='kPTString'>ADAT 1</property>
  <property id='kPropIOType' type='kPTInt'>3</property>
  ...
</mixer_object>
```

- `relative_index` — the channel ordering. This is the thing that could not be
  derived by reasoning, and it is stated explicitly.
- `kPropIOType` — the channel type.
- `kPropMixerTypeId` (on the `kMixer` object) — the model key.

Preamp channels additionally carry `kPropGain`, `kPropPad`, `kProp48VEnable`,
`kPropImpedance`, `kPropLowCut`, which lines up with the manual's preamp feature
list and with the driver's existing preamp controls.

### Console IOType enumeration

| `kPropIOType` | Channel class |
|---|---|
| 0  | ANALOG (mic/line preamp) |
| 2  | S/PDIF |
| 3  | **ADAT** |
| 7  | VIRTUAL |
| 12 | UNAVAILABLE (placeholder strip) |
| 13 | TALKBACK |

### Twin class map (`kPropMixerTypeId = 2`)

Document at offset 18754864. 22 inputs, 7 outputs, 2 aux buses:

```
IN[ 0] IOType=0   ANALOG 1
IN[ 1] IOType=0   ANALOG 2
IN[ 2] IOType=3   ADAT 1
IN[ 3] IOType=3   ADAT 2
IN[ 4] IOType=3   ADAT 3
IN[ 5] IOType=3   ADAT 4
IN[ 6] IOType=3   ADAT 5
IN[ 7] IOType=3   ADAT 6
IN[ 8] IOType=3   ADAT 7
IN[ 9] IOType=3   ADAT 8
IN[10] IOType=2   S/PDIF L
IN[11] IOType=2   S/PDIF R
IN[12] IOType=7   VIRTUAL 1
IN[13] IOType=7   VIRTUAL 2
IN[14] IOType=7   VIRTUAL 3
IN[15] IOType=7   VIRTUAL 4
IN[16] IOType=7   VIRTUAL 5
IN[17] IOType=7   VIRTUAL 6
IN[18] IOType=7   VIRTUAL 7
IN[19] IOType=7   VIRTUAL 8
IN[20] IOType=13  TALKBACK
IN[21] IOType=12  UNAVAILABLE

OUT[0] HP        OUT[1] LINE 3/4   OUT[2] CUE 3
OUT[3] CUE 4     OUT[4] MONITOR    OUT[5] HP      OUT[6] LINE 3/4
```

This corroborates the Apollo Twin X Hardware Manual (v210429, p.7–8) exactly:
two mic/line preamps, one Hi-Z instrument input (channel 1 only), six D/A
channels via stereo monitor + stereo headphone + line outputs 3-4, up to eight
channels of ADAT optical input with S/MUX *or* two channels S/PDIF, two
independent stereo aux buses, and a built-in talkback mic.

Corroborated independently by the running Mixer Engine log on the test machine:

```
Twin X' (hwID = 1202099349)
```

### What the description establishes

**Closes:** the channel composition and ordering — the part previously called
un-derivable. It is authoritative, model-specific, and free to read.

**Does not close:** the Console `kPropIOType` enumeration is **not** the same
enumeration as the driver's routing `channel_id` high byte. Compare:

| Class | driver `channel_id` high byte (from x4 tables) | Console `kPropIOType` |
|---|---|---|
| MIC/LINE/HIZ | 0x00 | 0 |
| MIC/LINE | 0x01 | 0 |
| AUX | 0x02 | — |
| LINE | 0x03 | — |
| DSP internal | 0x04 | — |
| S/PDIF | 0x05 | 2 |
| VIRTUAL | 0x07 | 7 |
| MON | 0x09 | — |
| CUE | 0x0a | — |
| TALKBACK | 0x0b | 13 |
| **ADAT** | **unknown** | 3 |

VIRTUAL coincides at 7; S/PDIF and TALKBACK do not. So these are two related but
distinct enumerations, and Console's value cannot be substituted directly.

**The single remaining unknown for routing is the driver-level ADAT type byte.**
The x4 tables contain no ADAT entry, so it appears nowhere in what we have.
Observed driver type bytes are 0x00–0x05, 0x07, 0x09–0x0b, leaving **0x06** and
**0x08** as the gaps — and 0x06 sits directly adjacent to S/PDIF (0x05), which
is suggestive. That is a two-candidate sweep, not an open question.

### Why this changes the capture plan

Gap 3 was the reason to disable SIP. With the composition and ordering now
readable from disk, DTrace is reduced from *required* to *confirmatory* — useful
for verifying the ADAT byte and for gap 4 (DSP program blocks), but no longer
the only path. That is a materially better security tradeoff than the one
originally on the table.

### Read the embedded descriptions

```bash
python3 - <<'EOF'
import re
p="/Library/Application Support/Universal Audio/Apollo/UA Mixer Engine.app/Contents/MacOS/UA Mixer Engine"
d=open(p,"rb").read()
for m in re.finditer(rb"<\?xml version='1\.0'", d):
    off=m.start(); blob=d[off:off+70000].decode("utf-8","replace")
    if "kRoot" not in blob: continue
    tid=re.search(r"kPropMixerTypeId' type='kPTInt'>(\d+)<", blob)
    ins=re.findall(r"type='kInput' relative_index='(\d+)'.*?kPropName' type='kPTString'>([^<]*)<.*?kPropIOType' type='kPTInt'>(-?\d+)<", blob, re.S)
    seen=set(); rows=[]
    for i,n,t in ins:
        if i in seen: continue
        seen.add(i); rows.append((i,t,n))
    print(f"--- off={off} mixerTypeId={tid.group(1) if tid else '?'} inputs={len(rows)}")
    for i,t,n in rows: print(f"   IN[{i:>2}] IOType={t:>2}  {n}")
EOF
```

Read-only. Reads a file already on disk; touches no hardware and no security
settings.
