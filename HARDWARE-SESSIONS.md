# Hardware sessions: plug-in hosting phases A, S and L (owner-attended)

Branch `feat/plugin-hosting` (private; never merged into main, never pushed
outside the private dev hub). Everything below was prepared without touching
the Apollo: no module was loaded, no device node opened, no hardware script
run. Each session is for the owner to run, in this order, in one sitting if
you like (about two hours with the power cycles).

## What the build is

- Source: `~/github/oa-hosting/driver`. Build: `make -C ~/github/oa-hosting/driver`.
  Module: `~/github/oa-hosting/driver/ua_apollo.ko`. Check
  `modinfo -F srcversion ~/github/oa-hosting/driver/ua_apollo.ko` and write
  the value down; at commit `7bb57ee` it was `3B0AE2C87A49D9174FAD2CC`
  (a later commit changes it).
- Installed main driver (the rollback target):
  `/lib/modules/$(uname -r)/updates/dkms/ua_apollo.ko.zst`, srcversion
  `F56EA491C868E0EBEA9C8DF` when this was written
  (`cat /sys/module/ua_apollo/srcversion` while it is loaded).
- New module parameters, all off by default:
  - `plugin_host=1` serves `UA_IOCTL_BOARD_QUERY` (0x90) and
    `UA_IOCTL_PLUGIN_INSTANCE` (0x94);
  - `input_strips=1` builds the Mic Input Strips on DSP 0 after a cold start;
  - `input_strips_steps=N` (bring-up bisect: 1 code pages only, 2 + zero-fill,
    3 + memspec, 4 + header, 5 + parameters, 6 + SYNCH, 0 = all);
  - `input_strips_form=1` sends code pages the way UAD2System sends a
    registered record instead of the titan recipe's form;
  - `input_strips_rebuild` (writable, root) takes the live strips down and
    builds them again.
- With neither `plugin_host` nor `input_strips`, the driver reads nothing more
  and sends nothing more than main does; GET_VERSION's features word is
  main's and GET_PLUGIN_CAPS reports no operation.
- The helper for these sessions, `~/github/oa-hosting/tools/plugin-host-check.py`
  (`version`, `caps`, `query`, `check`, `proc`), opens `/dev/ua_apollo0`. It
  never prints the authorization table, only counts and the states you ask
  for, and `check` uses `CREATE_F_CHECK`: nothing is placed or uploaded.
- Readouts: `/proc/asound/x4/plugin_host` (ownership, pool registers and
  usage, everything the host sent per DSP, transactions, catalog, queries)
  and the `input strips` block at the end of `/proc/asound/x4/dsp_state`.
  The plan says debugfs; the driver's existing proc files are used instead.

## Before every session

```sh
# a second terminal: the kernel log, filtered
sudo dmesg -wT | grep -E 'ua_apollo|0000:44:00.0|AER|DMAR|frame counter'

# stop everything that holds the Apollo; the runtime mask keeps udev from
# restarting the daemon when the node reappears (it pulled PipeWire back in
# on titan)
systemctl --user mask --runtime ua-mixer-daemon.service
systemctl --user stop ua-mixer-daemon.service wireplumber.service \
    pipewire-pulse.service pipewire.service pipewire-pulse.socket pipewire.socket

# the module stores the strips and the queries read
ls /lib/firmware/universal-audio/dsp/          # 0a5 0c2 0c9 0da 0db 0eb 12b .bill
ls /lib/firmware/universal-audio/plugins/manifest.json
```

The runbook from the plan, where a session says "runbook": dmesg shows no
AER, DMAR or "frame counter still zero"; `~/oa-test/daemon_hwtest_4711.py`
20/20 with the daemon running as for that test;
`python3 ~/github/oa-hosting/tools/daemon-protocol-check.py --write --hardware`
13/13; `~/oa-test/tools/ratecheck.sh` at 48000, 192000, 44100, 48000, all
PASS (PipeWire stopped).

## Rollback (at any point)

```sh
sudo rmmod ua_apollo            # the test module: takes its strips down first
sudo modprobe ua_apollo         # the installed main driver
cat /sys/module/ua_apollo/srcversion          # F56EA491C868E0EBEA9C8DF
systemctl --user unmask --runtime ua-mixer-daemon.service
systemctl --user start pipewire.socket pipewire-pulse.socket \
    wireplumber.service ua-mixer-daemon.service
```

- If the x4 dropped off the bus (`lspci -s 44:00.0` empty, link-down or AER
  lines): `rmmod` may hang. Reboot, power-cycle the Apollo, then load main.
- Never use `full_shutdown=1` (it makes the next load cold-boot the DSPs).
- Analog inputs after a power cycle: with any Linux driver loaded, a power
  cycle leaves the analog inputs silent until input strips exist (session 3
  builds them; otherwise Console on macOS/Windows does, and main keeps them
  on a warm switch). Outputs and playback are not affected.
- A power cycle means: `sudo rmmod ua_apollo`, Apollo off for 10 s, on, wait
  until `lspci | grep 1a00:0002` lists it again (the address may change; then
  replace 44:00.0 below), then load.

## Session 1: A1, the ABI with the defaults (warm, no power cycle, 10 min)

```sh
sudo rmmod ua_apollo
sudo insmod ~/github/oa-hosting/driver/ua_apollo.ko
python3 ~/github/oa-hosting/tools/plugin-host-check.py version
python3 ~/github/oa-hosting/tools/plugin-host-check.py caps
```

Expected:
- dmesg: the usual warm probe ("mixer DSP alive! Skipping DMA reset"), no
  `plug-in host:` and no `input strips:` line.
- `version`: `ABI 1.8`, features exactly main's (titan reported 0x0003f7ff
  with `bus_solo_sign=0`), `features2 0x00000000 (plugin_host off)`.
- `caps`: `operations: none`, `owned 0x0, quarantined 0x0, instances 0`.
- `cat /proc/asound/x4/plugin_host`: `plugin_host=0, inactive`.
- Runbook green.

Failure: a features word that differs from main's, any operation bit, any
AER. Roll back and report.

## Session 2: A4 + L1/L2, ownership, pool map and the device's answer (cold, `plugin_host=1`, 20 min)

```sh
sudo rmmod ua_apollo
# power-cycle the Apollo (see above)
sudo insmod ~/github/oa-hosting/driver/ua_apollo.ko plugin_host=1
cat /proc/asound/x4/plugin_host
python3 ~/github/oa-hosting/tools/plugin-host-check.py caps
```

Expected (A4):
- dmesg: `plug-in host: cold start, DSPs this driver may use: 0xf (DSP 0
  boot word 0x00000003)`. A DSP whose boot word is not exactly 3 is not
  owned: write down DSP 1-3's boot words (this is the first time they are
  read after a power cycle).
- `plugin_host`, DSP 0: `owned`, `registers: size 00004000 0000a9b8 00001ffe
  00ffe000 base 000e0000 00092800 000c5000 08000000 scratch 00000080 00000080
  00008000` on titan's x4 (A6.1).
- Pool lines per DSP, compared with the Mac's per-DSP figures for an empty
  DSP (code 16384; pools 43320 / 8062 / 16699822 after scratch 128 / 128 /
  32768): pool 0 `total 16384 scratch 0`, pool 1 `scratch 128 free 43320`,
  pool 2 `scratch 128 free 8062`, pool 3 `scratch 32768` and its free words
  (titan's DSP 0 register gives 16736256, the Mac's DSPs 2/3 16699822: write
  down what each DSP says). `used 0` everywhere, every `sent:` counter 0.
- `caps`: `operations: BOARD_QUERY CREATE DESTROY`, `owned 0xf`.

Then the device's answer (L1, L2):

```sh
python3 ~/github/oa-hosting/tools/plugin-host-check.py query \
    0x806 0x830 "UAD UA 1176LN Legacy" "UAD 4K Buss Compressor"
python3 ~/github/oa-hosting/tools/plugin-host-check.py check 0x806 0x830
cat /proc/asound/x4/plugin_host
```

The query sends `{0x00100002, date}` and `{0x00110002, request}` to DSP 0, as
UA's own driver does on every refresh (it updates the device's date).

Expected:
- `query`: counts per state close to what UA Console showed for this Apollo
  (Mac capture `auth_now.json`: 138 authorized, 78 demo not started, 6 demo
  expired, 1 demo running; it may have changed since), `0x806: authorized`,
  `0x830: demo not started`. Compare a few more with UA Console's plug-in
  list if you can.
- `check`: `0x806: admitted (authorized)`, `0x830: refused (demo not
  started)`.
- dmesg: `plug-in host: plug-in 0x806 admitted by the device (state 0)`,
  `plug-in host: plug-in 0x830 refused (state 2, -13): nothing placed`.
- `plugin_host`: `authorization: 3 board queries (0 failed), 1 admitted, 1
  refused`, and every DSP still `uploads 0`, `used 0`: zero upload traffic.
- Runbook green (analog inputs are silent after this power cycle: expected).

Failure signs and what to record:
- `query` errors: `Timer expired` / `Protocol error` with a dmesg line
  `plug-in host: DSP 0 transaction (2 entries, 1 replies) ended ... reply
  word 0x...` — copy it.
- Any state that differs from what UA Console shows for the same plug-in:
  stop here. The gate decides from this table; it must match UA's software
  before anything else is built on it.

## Session 3: S1, the input strips (cold, `input_strips=1`, 30-45 min)

Start with the code pages alone, which kept the x4 up on titan, so the new
reply wait is seen before the full strip (which took the x4 off the bus in
0e9990b's run). Every attempt needs its own power cycle: a DSP that any load
has used is not the driver's again until the next power cycle.

```sh
sudo rmmod ua_apollo
# power-cycle
sudo insmod ~/github/oa-hosting/driver/ua_apollo.ko input_strips=1 plugin_host=1 input_strips_steps=1
sleep 3; cat /proc/asound/x4/plugin_host; tail -30 /proc/asound/x4/dsp_state
```

Expected with `input_strips_steps=1`:
- dmesg: `input strips: enabled for 4 analog input(s) (mask 0xf); modules
  0x0eb 0x0da 0x0a5 0x0c2 0x12b read`, the cold-start line, then (0.5 s after
  the connect) `input strips: DSP0 pools empty, creating 4 Mic Input
  Strip(s) at 48000 Hz`.
- `plugin_host` DSP 0: `uploads 5 (replies ok 5, bad 0)`, `transactions 5`,
  pool 0 `used 994 holds 5`, nothing quarantined.
- dsp_state: modules `resident at 0xe0000`, `0xe006c`, `0xe025c`, `0xe038c`,
  `0xe03a2`.
- If a module is refused: `DSP 0 refused module 0x0eb: reply ...` and DSP 0
  quarantined. If no reply comes: `transaction (1 entries, 1 replies) ended
  -110 ... cmd end X pos X, resp end Y pos Y-1, reply word 0x...`. Record the
  line, power-cycle and retry with `input_strips_form=1` (the record sent
  without the architecture byte in the id, as the genuine 1176 uploads were
  answered on titan).

Then, each after a power cycle: `input_strips_steps=6` (everything up to the
SYNCH, without DSP 0's span/route words), then the full strip
(`input_strips_steps` left out):

```sh
sudo insmod ~/github/oa-hosting/driver/ua_apollo.ko input_strips=1 plugin_host=1
```

Expected (full strip):
- dmesg: four `input strips: input N: Mic Input Strip at 0x9d1b0 / 0x9d15c /
  0x9d108 / 0x9d0b4 on DSP 0 (taken after X ms)`.
- `lspci -s 44:00.0` still lists the x4; no AER, link-down or DMAR line.
- `plugin_host` DSP 0: `uploads 5 (replies ok 5)`, `zero-fills 40, memspecs
  4, writes 8`; pool 1 `used 336 scratch 448`, pool 2 `used 64`, pool 3
  `used 4160`; `transactions: 0 held` after a few seconds.
- dsp_state: `state live`, `pools: empty: cold-started by this driver ...`,
  four strips `live`.
- Analog inputs 1-4 reach the record channels: with a signal on each input,
  the record channels `cat /proc/asound/x4/io_map` names MIC/LINE 1-4 carry
  it (`arecord -D hw:CARD=x4,DEV=0 -c 22 -f S32_LE -r 48000 -d 3 /tmp/in.wav`
  and look at those channels), and readback words 8-25 are nonzero
  (`sudo ~/oa-test/tools/uactl rb`).
- Rate sweep (each change takes the strips down and builds them again):
  `for r in 44100 48000 88200 96000 176400 192000; do ~/oa-test/tools/ratecheck.sh $r; done`;
  per rate dmesg `4 strip(s) stopped, modules released (rate change)` then
  `creating 4 Mic Input Strip(s) at R Hz` and the four strip lines; inputs
  alive at every rate.
- Warm reload, by design: `sudo rmmod ua_apollo` logs `4 strip(s) stopped,
  modules released (remove)`; `sudo insmod ... input_strips=1 plugin_host=1`
  without a power cycle logs `plug-in host: warm start, DSPs this driver may
  use: 0x0` and `input strips: DSP0 pools in use, leaving the existing
  strips (running when this driver found it ...)`: no strips, inputs silent
  until the next power cycle.
- Hot-replug (Thunderbolt cable out and in, Apollo powered): the probe is
  warm, as above. Apollo off and on while the cable stays: cold, strips
  rebuilt.
- Runbook green with the strips live.

If the x4 drops off the bus: record the last dmesg lines and which step
setting was loaded, recover (rollback section), and continue the bisect
with `input_strips_steps=2`, `3`, `4`, `5` (one power cycle each) to find the
first step that drops it; `input_strips_mask=0x1` builds one strip only.

## Session 4: S2, 50 create/teardown cycles (continue from a working full-strip load, 10 min)

```sh
for i in $(seq 50); do
    echo 1 | sudo tee /sys/module/ua_apollo/parameters/input_strips_rebuild >/dev/null
    sleep 0.2; grep -A3 '^DSP 0:' /proc/asound/x4/plugin_host | grep 'pool 1:'
    sleep 3
done
grep -A8 '^DSP 0:' /proc/asound/x4/plugin_host; grep transactions /proc/asound/x4/plugin_host
```

Expected:
- Right after each write, DSP 0 pool 1 `used 0` (the baseline: the teardown
  frees the ledger as it publishes); three seconds later the strips are back.
- dmesg: 50 x `4 strip(s) stopped, modules released (rebuild requested)` and
  50 x the four strip lines; no `quarantined`, no AER, the x4 stays up.
- At the end: `transactions: 0 held`, `releases` = 5 x 50, `stops` = 4 x 50
  more than before, DSP 0 not quarantined.
- `sudo rmmod ua_apollo`: `4 strip(s) stopped, modules released (remove)`,
  no `teardown was not taken`.

## Session 5: L3, the daemon's plug-in status (any `plugin_host=1` load, 10 min)

With the test module loaded with `plugin_host=1` (session 2 or 3 load), run
the branch's daemon in a terminal instead of the installed one (the service
stays masked), with the UA catalogue imported as usual:

```sh
cd ~/github/oa-hosting/mixer-engine && python3 ua_mixer_daemon.py
# in another terminal (the daemon listens on 4710 for /dev/ua_apollo0)
T=~/github/oa-hosting/tools/oa-daemon-client.py
python3 $T --port 4710 get /plugins/0/Name/value
python3 $T --port 4710 get /plugins/0/Status/value
```

Expected:
- daemon log: `Plug-in host on: licence status of N plug-ins from the device
  (M with a render id)` and `Plug-in licences from the device: A authorized,
  D demo running, S demo not started, E demo expired, ...` (counts only;
  the table is never logged).
- For the 1176LN Legacy entry: `Authorized` true, `Status` `Authorized for
  all devices`, `Demoing` true (what the Mac engine reported for authorized
  plug-ins); for the 4K Buss Compressor: `Authorized` false, `Status` `Demo
  not started`, `Demoing` false.  Find their N with `get /plugins/N/Name/value`
  (catalogue order).  Compare a few with UA Console's plug-in list.
- `post /PluginStartDemo` (from a client) fails: demos are started in UA's
  own software, open-apollo never starts one.
- A rate change (session 3's sweep) or a reconnect blanks the status and the
  daemon asks again (at most once per 5 s).
- With a driver loaded without `plugin_host=1`, nothing changes: Status
  stays empty as before.

Stop the daemon (Ctrl-C) before the rollback.

## Not in these sessions

No plug-in instance is created (only `CREATE_F_CHECK`): placing and
uploading a plug-in's own modules on DSPs 1-3, chains, routes and
parameters are Phase P (P3 and later) and get their own sessions.

## Results, 2026-10-13 (titan's x4, owner present)

Driver as of this commit (input strips and plug-in host on; defaults below).

- **Session 1** (defaults, warm): passed. ABI 1.8, features 0x0003f7ff as main,
  features2 0, no operations, zero traffic.
- **Session 2** (cold, `plugin_host=1`): passed. All four DSPs boot word 3, owned
  0xf; pool geometry equals UA's Mac figures (free 43320 / 8062 / 16699822).
  Board query: 134 authorized, 48 demo not started, 82 demo expired, 504
  unavailable (the device's whole 768-entry table); 0x806 admitted, 0x830
  refused (state 2), zero uploads.
- **Native 1176 CREATE on DSP 3** (not in the plan; one-off script
  `create1176.py` in the private plugin-hosting folder, inputs from
  runs/genuine-1176-native-allocation-20261011-2): 4 modules uploaded and
  acknowledged, code at 0xe0000 as UA places it, 6 private rows zero-filled with
  one memspec; DESTROY released everything. Not routed or started.
- **Session 3** (input strips): passed after four fixes, now the defaults:
  1. `input_strips_form=1`: the arch byte must be cleared in the module id
     (form 0 was refused: reply `80070004 f0010005 020000eb ...`, DSP quarantined).
  2. `rate_ack_real=1`: a DSP not told the rate (`{0x001A0002, rate}`) stalls on
     the strip's SYNCH.
  3. `input_strips_params=1`: UA's captured layout (m0[0..17], m0[38..53],
     m3[1..15]); the A1 layout started the strip and hung the x4 (BAR0 all ones,
     DLL replay timeouts on 43:01.0) about 10 ms later.
  4. `input_strips_dsp=1`: strips on DSP 0 feed the record channels but not the
     monitor mix; on DSP 1 both.
  Plus the phantom resync (`input_strips_phantom`, `ua_preamp_phantom_resync()`):
  after a cold start a preamp the Apollo reports with 48V on stays muted until
  48V is written off then on; done once per cold start, never where 48V was off.
  Result: an SM7B on input 1 records and reaches MON L/R from a cold start with
  nothing touched; rates 44.1/96/192/48 kHz all pass (strips rebuilt per rate).
- **Session 4** (50 rebuilds): passed. Pool 1 back to 0 after every teardown,
  275 uploads all acknowledged, 0 timeouts, 0 held or leaked transactions.
- **Open:** input 2 reads digital zero with nothing plugged in, whatever is
  toggled; test it with a source. Session 5 (daemon licence status) not run.

### How these sessions were run (titan)

- Power: Home Assistant Kasa outlet `switch.studio_apollo` over `ssh ha`
  (turn_off, 10 s, turn_on; the x4 is back in about 8 s).
- udev loads the installed DKMS `ua_apollo` the moment the x4 enumerates, even
  with a /run/modprobe.d blacklist; load the test module while the x4 is off:
  power off, `rmmod`, `insmod` the test .ko, power on.
- Stop and mask the audio stack first: `pipewire.socket`, `pipewire`,
  `wireplumber`, `pipewire-pulse(.socket)`, `pipewire-aes67` (`--runtime`) and
  `ua-mixer-daemon` (its unit is in /etc/systemd/user, so mask it without
  `--runtime`, and unmask it afterwards); the udev worker restarts the daemon.
- Measure with `arecord -D hw:CARD=x4,DEV=0 -c 22 -f S32_LE -r 48000` and the
  per-channel RMS; ALSA ch 0-3 are inputs 1-4, ch 14/15 MON L/R.

## Results, 2026-10-14: corrected 1176 insert

The pending HANDOFF.md section 6 run passed on the Apollo x4 at 48 kHz.
Source: private `feat/plugin-hosting` at `ba1a24a`; loaded module srcversion
`EBCB6AAB18002A16F1E8F35`. No source changes or pushes were needed.

### Observed

- Cold-loaded the corrected driver with `input_strips=1 plugin_host=1`.
  All four strips started on DSP 1. No AER failure occurred.
- Offline checks passed for 16 rows: default 11 WRITEs / 160 words;
  Input 0.8: 12 WRITEs / 166 words.
- Both CREATE+START calls completed in approximately 11 ms, after the device
  authorization gate admitted the plug-in. Both instances were destroyed.
- The default insert sustained audio through its 20-second hold. Input 0.8
  sustained audio through its 10-second hold. Inputs 3/4 retained their
  approximately -107 dBFS noise floor throughout.
- One explicit strip rebuild after removal succeeded. Input 1 and monitor
  audio returned without another cold cycle or a DSP timeout.
- After rebuild: all four DSPs owned, zero quarantined DSPs, zero instances,
  zero parked DMA buffers. All five previously active audio services were
  unmasked and restarted successfully. The corrected hosting driver remains
  loaded with working strips.

### Measurements

Two-second recordings, RMS dBFS. Source was the live input-1 mic/room signal,
which varied between recordings; these are not calibrated gain measurements.

| Stage | Input 1 | Monitor L/R |
|---|---:|---:|
| Dry immediately before default insert | -52.0 | -55.0 |
| Default insert, start | -49.2 | -52.2 |
| Default insert, end | -42.6 | -45.6 |
| Input 0.8 insert, start | -30.5 | -33.3 |
| Input 0.8 insert, end | -34.3 | -37.2 |
| Plain strips restored | -51.1 | -54.1 |

The first samples differ by +18.7 dB between default and Input 0.8, consistent
with the Mac reference but not proof of exact gain parity. Input 2 remained
digital zero, as previously observed with no source connected.

The second script labels its pre-insert measurement "dry (plain strip)".
That label is incorrect in this sequence: `--no-restore` had removed the
first chain without rebuilding input 1. Its zero reading is expected and
must not be used as a dry gain reference.

## Results, 2026-10-14: live controls, restoration, native daemon

Private source commits: `3614d9f` (INSTANCE SERVICE and retained-strip restore),
`18c1bba` (native daemon backend). Tested module srcversion
`C5EA0ACFCFA057CEF51FCE4`; Apollo x4, 48 kHz. No pushes.

- SERVICE validates retained row/link bounds, owner and epoch, then delivers live
  parameter writes and validated private-row reads. A generic borrowed-fd Python
  client and native daemon backend now exercise that API.
- DESTROY and fd close restore only the affected retained plain strip. Stop-only
  transactions prove their fence; pending inserts/fences defer strip rebuild.
  START is deliberately restricted to mono, the matching live-strip DSP, and
  one running insert per DSP.
- All seven 1176LN Legacy controls and all five ratios passed exact hardware
  coefficient readback. Private producer validation covered 1405 stateful cases
  and 112400 exact coefficient comparisons against the genuine emulated setter.
  Runtime requires no Wine or UA DLL; no vendor pages enter this repository.
- Stable Input/Output gain sweeps matched expected changes within 0.002 dB.
  Meter comparisons at calibrated input levels differed from the captured UA
  reference by 0.29–2.78% (+4/+8) and 7.55% (GR at -12 dBFS). This bounded agreement
  compares a mono insert with the retained stereo reference.
- Four separate carriers reached the four analog inputs: 700, 1100, 1500, 1900 Hz.
  Eight cycles covered every input using both DESTROY and owning-fd close, with
  336 live updates. Other carriers changed at most 0.042 dB; restored levels at
  most 0.032 dB. Pool state returned exactly to baseline after every cycle, with
  no global rebuild, timeout, quarantine, or AER failure.
- Burst audio established distinct Attack/Release response. The original recipe
  also added exactly 800 samples of relative recording delay: its Mic strip
  compensation is 1000 versus the plain strip's 200. Recovered source identifies
  the field; a single-field hardware test removed/restored exactly 800 samples.
  The private 48 kHz mono bench provider now verifies delay 200 while retaining the
  captured 2048-word buffers. This is not a general compensation policy.
- The real engine/helper protocol passed load, all sevencontrols, all ratios, fresh
  meter reads, refused unsupported mutations, unload, daemon exit with a live
  insert, and restart without restoring a stale lease. The final provider
  passed again with the imported catalogue. A native daemon-created insert
  measured 0added samples relative to the simultaneously driven plain input.
- Native GUI interaction, stereo/multiple inserts, other rates/plug-ins,
  reconnect replay, and distribution-ready provider generation remain open.
  See `mixer-engine/NATIVE-HOSTING.md` for the opt-in contract.

Validation: 131 driver tests; 2004 mixer-engine tests plus 89 subtests; module build
and diff check passed. Earlier harness failures (quiet-source broadband noise,
insufficient meter settling, helper query shape, one ALSA capture overrun) retain
private artifacts and were corrected before the passing runs above.

End state: normal daemon/PipeWire/WirePlumber/Pulse/AES67 services restored,
all tones and lab daemons stopped, all four DSPs owned, zero instances, zero held,
awaiting, or leaked transactions. Tested module remains loaded; DKMS unchanged.

## Results, 2026-10-19: record tap flag (UAD REC / MON bench)

Mac Studio (Asahi, `7.1.13-1-tbt`), Apollo x4 over Thunderbolt, 48 kHz, DKMS `ua_apollo`
as installed; owner present. Script: `tools/rectap-bench.py` (the daemon's own composer,
driver client and the user-built `UAD UA 1176 Rev A` mono bundle). Daemon and PipeWire
stopped for the run; recorded with `arecord` (22 ch), RMS from 1 s on. Input 1 carried a
steady room/mic signal at about -40.9 dBFS. Question and background:
`mixer-engine/RECORD-PRE-EFFECTS.md`.

The two recipes differ in exactly two words: Mic strip `m0` word 4 (row 1) and the
1176's word 4 (row 10), i.e. where bit `0x4` sits.

| Step | Bit `0x4` on | Input 1 record (ch 0) |
|---|---|---:|
| B0 plain strip | Mic `m0` (plain recipe) | -40.85 |
| C chain as composed today, 1176 Input 0.8 | 1176 | **-17.44** (wet) |
| L same chain, flag moved live by SERVICE | Mic `m0` | **-40.91** (dry) |
| L2 flag moved back live | 1176 | **-17.47** (wet) |
| B1 chain destroyed | plain | -40.88 |
| F fresh chain built with the flag on `m0`, Input 0.8 | Mic `m0` | **-40.82** (dry) |
| B2 chain destroyed | plain | -40.86 |

- **Proven:** the block holding bit `0x4` of word 4 is the record tap. On the Mic strip the
  record channel is dry to within 0.1 dB of the plain strip; on the last effect it is wet.
  The flag works at CREATE and live through SERVICE, both directions, with readback
  matching, no fault, no quarantine, no kernel message beyond the two placements.
- **Not measured:** the monitor side. MON L/R did not correlate with input 1 in any step
  (it followed input 2's source), so this run cannot tell "record dry, monitor wet" (the
  REC/MON switch) from "the whole chain output dry". Rerun with input 1 alone in the
  monitor mix (unmuted, soloed) and nobody else driving the mixer.
- Side effect to know: restarting the daemon afterwards restored its saved state file
  (input 1 muted, preamp gain 65 dB), not the live state it had before the stop.

End state: daemon, PipeWire, WirePlumber and Pulse running; zero bench instances (fd
closed after both destroys); Console reconnected.

Monitor-side rerun attempt (20:10, not run): with input 1 unmuted and soloed through the
daemon and PipeWire stopped, MON L/R carried a steady -50 dBFS signal that correlated with
none of the 22 record channels (peak |r| ≤ 0.01), input 1 recorded -72 dBFS. Meanwhile other
sessions' `pytest` runs held `/dev/ua_apollo0`, a native instance (plug-in 0x894) had been
started at 20:02 by an unknown owner, the driver had rebuilt all strips at 19:57 (now with
Virtual 1–4 strips) and input 1's preamp gain changed twice without the bench. The bench was
not run; input 1 was put back (muted, solo off). The monitor half needs the rig to itself:
no other device holders (`/proc/*/fd`), no native instances, MON correlating with input 1
on the plain strip (`tools/rectap-bench.py` now reports `mon_vs_rec_corr`).

Monitor side, rerun 20:15 (rig to itself: only the daemon held the device, no tests). The
daemon kept running; input 1 unmuted and soloed, input 2 (the Console's Ampeg SVTVR) muted;
PipeWire stopped. On the plain strip MON L was an exact copy of input 1 (r = 1.000,
-17.73 dB: fader -14.8 plus pan law). Same `tools/rectap-bench.py` steps, input 1 at about
-66 dBFS:

| Step | Bit `0x4` on | Record | MON L | MON vs record |
|---|---|---:|---:|---|
| B0 plain | Mic `m0` | -66.26 | -83.99 | r 1.000, -17.73 dB, lag -194 |
| C chain, 1176 Input 0.8 | 1176 | **-28.51** | **-46.26** | r 1.000, -17.73 dB, lag -194 |
| L live flip | Mic `m0` | **-66.24** | **-46.24** | r 0.984, +19.86 dB, lag -139 |
| L2 live flip back | 1176 | -28.43 | -46.15 | r 1.000, -17.73 dB, lag -194 |
| B1 destroyed | plain | -66.31 | -84.02 | r 1.000, -17.73 dB |
| F fresh chain, flag on `m0` | Mic `m0` | **-66.25** | **-46.26** | r 0.984, +19.86 dB, lag -139 |
| B2 destroyed | plain | -66.20 | -83.93 | r 1.000, -17.73 dB |

- **Proven: the flag is UAD REC / MON.** On the 1176 the record and the monitor are both
  wet (REC). On the Mic strip the record is dry, identical to the plain strip, while the
  monitor stays wet at the same level (MON). Live through SERVICE, or at CREATE, both ways.
- The dry tap is 55 samples earlier than the wet path (lag -194 → -139): the 1176 Rev A
  chain adds 55 samples, while `native_known_good.json` lists its latency as 0.
- Mixer restored to the snapshot taken before (inputs 1–4 mute, solo, fader, gain
  identical); PipeWire, WirePlumber, daemon running; zero bench instances; no kernel faults.

## Session: inserts on the Virtual strips (Mac Studio, cold, 30 min)

What it checks: a plug-in on Virtual 1 processes what a DAW plays to VIRTUAL 1 and the result
comes back on the VIRTUAL 1 record channel (the Apollo as a hardware insert), and how long the
round trip is. Mac Studio (Asahi, `7.1.13-1-tbt`), Apollo x4 at 48 kHz, plug-in maps in
`~/.local/share/open-apollo-plugin-maps`. `W` is the branch's checkout (a worktree here):

```sh
W=~/Developer/open-apollo/.claude/worktrees/silly-wu-082299
make -C $W/driver                                   # as rolo; the tbt kernel's build tree
modinfo -F srcversion $W/driver/ua_apollo.ko        # write it down
cat /sys/module/ua_apollo/srcversion                # the installed DKMS one (rollback target)
```

1. Stop everything that holds the Apollo (close uaconsole first). The runtime mask keeps udev
   from starting the installed daemon when the node comes back:

   ```sh
   systemctl --user mask --runtime ua-mixer-daemon.service
   systemctl --user stop ua-mixer-daemon.service wireplumber.service \
       pipewire-pulse.service pipewire.service pipewire-pulse.socket pipewire.socket
   ```

2. Load the branch's driver on a cold Apollo (hosting needs DSPs this driver cold-started):

   ```sh
   sudo rmmod ua_apollo
   # Apollo off, 10 s, on; wait until it is back (hot-plug is untested on the tbt kernel:
   # if it does not come back, reboot with the Apollo on)
   lspci -nn -d 1a00:
   sudo insmod $W/driver/ua_apollo.ko plugin_host=1 input_strips=1
   grep -A12 '^input strips' /proc/asound/x4/dsp_state   # state live, strips 4-7 = Virtual 1-4
   ```

3. PipeWire with the Virtual nodes, then the branch's daemon in a terminal (same flags as the
   unit's drop-in):

   ```sh
   systemctl --user start pipewire.socket pipewire-pulse.socket wireplumber.service
   bash $W/configs/pipewire/setup-apollo-io.sh          # adds Apollo Virtual Out/In 1+2, 3+4
   ~/.local/share/open-apollo/plugin-venv/bin/python3 $W/mixer-engine/ua_mixer_daemon.py \
       --native-plugin-bundles ~/.local/share/open-apollo-plugin-maps --preserve-hardware
   ```

   In another terminal: `python3 $W/tools/oa-daemon-client.py --port 4710 get
   /NativePluginHosting/value` is true, and a known-good plug-in's `NativeMonoSlots` lists
   `/devices/0/inputs/14/effects/0` (find it with `get /plugins/N/Name/value`).

4. Reference run, nothing on Virtual 1 or 2 (both strips plain). Turn the monitor down: the
   clicks also reach the monitor mix.

   ```sh
   python3 $W/tools/virtual-insert-roundtrip.py ~/oa-test/virtual-insert/plain
   ```

   Expect both Virtual inputs back at the same offset (`insert ... +0 samples`), tone `+0.00 dB`.

5. Load `UAD Pultec EQP-1A Legacy` (known good, mono, latency 0 in the list, about 31 samples
   measured on input 3) on Virtual 1: drop it in uaconsole's Virtual 1 insert, or
   `python3 $W/tools/oa-daemon-client.py --port 4710 --timeout 20 set /devices/0/inputs/14/effects/0/EffectName/value '"UAD Pultec EQP-1A Legacy"'`.
   The daemon log shows the load; `/proc/asound/x4/plugin_host` shows the chain placed. Then:

   ```sh
   python3 $W/tools/virtual-insert-roundtrip.py ~/oa-test/virtual-insert/pultec
   ```

   Pass: Virtual 1 comes back (no "no click came back"), Virtual 2 unchanged from step 4, and
   `insert on Virtual 1` reports the plug-in's own latency (about +31 samples expected). Turn a
   control (e.g. the Pultec's high boost) and run again: Virtual 1's tone change moves, Virtual
   2's does not.

6. Unload (clear the slot, or `--timeout 20 set .../effects/0/EffectName/value '""'`) and run step 4 again:
   Virtual 1 matches Virtual 2 again (the plain strip is back). Optional: link Virtual 1/2 and
   load a stereo bundle on Virtual 1; `--pair 34` checks Virtual 3/4.

7. Rollback: stop the branch's daemon (Ctrl-C), then

   ```sh
   systemctl --user stop wireplumber.service pipewire-pulse.service pipewire.service \
       pipewire-pulse.socket pipewire.socket
   sudo rmmod ua_apollo
   # Apollo off, 10 s, on (wait for lspci as above)
   sudo modprobe ua_apollo
   systemctl --user unmask --runtime ua-mixer-daemon.service
   systemctl --user start pipewire.socket pipewire-pulse.socket wireplumber.service \
       ua-mixer-daemon.service
   ```

Report: both `results.json`, the round trip (step 4: what a PipeWire client sees for a plain
Virtual strip; step 5: plus the insert), and anything in `journalctl -k | grep ua_apollo`.
