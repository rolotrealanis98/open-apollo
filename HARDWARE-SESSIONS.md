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

Filled in below once the daemon part is reviewed (see the end of this file).

## Not in these sessions

No plug-in instance is created (only `CREATE_F_CHECK`): placing and
uploading a plug-in's own modules on DSPs 1-3, chains, routes and
parameters are Phase P (P3 and later) and get their own sessions.
