# Testing the v2 driver on your Apollo

Thanks for testing. Two scripts, both run as your normal user from a checkout
of this repo, with the driver installed and the Apollo on and connected.

## 1. Collect a report (safe, read-only)

```bash
tools/apollo-report.sh
```

Writes `apollo-report-<date>.txt` in the current directory: kernel/distro,
module version and parameters, PCI link status (Apollo and Thunderbolt
bridges), ALSA card, `io_map`, mixer controls, driver kernel log lines, device
info and one hardware readback (decoded), PipeWire/WirePlumber state, and
whether the mixer daemon is listening. It never writes to the hardware.

Some systems hide `lspci -vv` link registers or `dmesg` from normal users. The
report says so; add `--sudo` to let it use `sudo` for just those two reads.

Not collected: hostname, username, serial numbers, network details. Read the
file before sending it.

## 2. Run the duplex self-test (plays a tone)

```bash
tools/apollo-selftest.sh        # appends its summary to the newest report
```

Turn your speakers and monitor level down first: it plays a -12 dBFS 997 Hz
tone on the MON outputs. It asks before each step:

1. Stop PipeWire/WirePlumber (restored when the script ends).
2. For each sample rate (44.1, 48, 88.2, 96, 176.4, 192 kHz): play the tone on
   playback channels 1/2 while recording every record channel, then check the
   MON L/R record channels (found by channel ID 0x0901/0x0902 from `io_map`).

Answer `n` to skip a rate, `a` to run the rest without asking, `q` to stop.
Options: `--rates "48000 96000"`, `--card N`, `--report FILE`, `--keep` (keep
the WAV files), `--yes` (no questions).

## 3. What to send

The report file (it already contains the self-test table), plus:

- your Apollo model, and what is connected to it (nothing, or what);
- anything odd you heard or saw (front panel, PipeWire errors);
- if the machine hung or the Thunderbolt link dropped, say at which step.

Open an issue on the repo and attach the file.

## What the verdicts mean

| Verdict | Meaning | Possible cause |
|---------|---------|-------------------|
| PASS | The tone came back on both MON channels. | Playback, capture and the monitor path work at that rate. |
| SILENT | Capture is live (other channels carry data) but MON is all zero. | MON capture slot is wrong for your model, or the monitor mix is muted or has no source. Check the monitor mute/level in the report's readback section. |
| DEAD | Nothing live: capture empty or too short, every channel zero, or a channel repeats the sample 8192 frames earlier (a frozen DMA buffer). | Capture DMA is not running or not advancing, or the DSP is not connected. Look at the kernel log section. |
| ROUTE | MON has data but not the tone on both channels: tone missing, on one side, or on a different record channel. | Channel map or routing mismatch for your model. The per-channel table in the report shows where the tone landed. |
| SKIP | Rate skipped by you, or the card does not accept it. | Not a failure by itself. |
| ERROR | `aplay`/`arecord` failed (busy device, unsupported channel count), or MON IDs were not in `io_map`. | The detail column has the tool's error message. If it says "busy", stop PipeWire and retry. |

No cables are needed: the tone is generated in software and the MON record channels are captured from the Apollo itself. Unplug or turn down anything connected to the outputs.

If a run hangs or ends in a crash, do not run it again at the same rate. Reboot
(a cold boot if the Thunderbolt link dropped), run only `apollo-report.sh`, and
send that.

## Developers

The tone generator, classifier and `io_map` parser are in
`tools/apollo-wavcheck.py`; `python3 tools/test-apollo-wavcheck.py` checks the
classifier on synthetic captures (tone, silence, stale repeating buffer, tone on
the wrong channel) without hardware.
