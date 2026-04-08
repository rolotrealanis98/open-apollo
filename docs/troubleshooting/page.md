---
title: Troubleshooting
---

This page covers common issues encountered when using Open Apollo on Linux, with symptoms, causes, and solutions for each.

---

## Driver Won't Load

### Module not found

**Symptom:** `modprobe ua_apollo` returns `Module ua_apollo not found`.

**Cause:** The module is not installed in the kernel module tree.

**Fix:**
```bash
cd driver
sudo make install
# Or load directly without installing:
sudo insmod driver/ua_apollo.ko
```

### Missing firmware

**Symptom:** `dmesg` shows:
```
ua_apollo: firmware file ua-apollo-mixer.bin not found
```

**Cause:** The mixer DSP firmware blob is not in `/lib/firmware/`.

**Fix:** The firmware must be obtained from an existing macOS or Windows UAD installation. See [Building from Source — Step 4](/docs/building-from-source#step-4-install-firmware) for extraction instructions. Then place it at `/lib/firmware/ua-apollo-mixer.bin`.

### Kernel version mismatch

**Symptom:** `insmod` returns `Invalid module format` or `disagrees about version of symbol`.

**Cause:** The module was compiled against different kernel headers than the running kernel.

**Fix:**
```bash
cd driver
make clean && make
sudo insmod ua_apollo.ko
```

After every kernel update, rebuild the module. Consider setting up [DKMS](/docs/building-from-source#dkms-setup) for automatic rebuilds.

### Module conflicts

**Symptom:** Device is claimed by another driver, or `insmod` returns `Device or resource busy`.

**Cause:** Another driver has already bound to the PCI device.

**Fix:**
```bash
# Check what's bound to UA devices (vendor 0x1a00)
lspci -d 1a00: -k

# If another driver is bound, unbind it first
echo "0000:xx:00.0" | sudo tee /sys/bus/pci/drivers/<other_driver>/unbind
sudo insmod ua_apollo.ko
```

---

## Device Not Detected

### Apollo not visible on PCIe bus

**Symptom:** `lspci -d 1a00:` returns nothing.

**Causes and fixes:**

1. **Thunderbolt not authorized:** Modern Linux requires explicit Thunderbolt device authorization.
   ```bash
   # Check Thunderbolt devices
   cat /sys/bus/thunderbolt/devices/*/authorized

   # Authorize all pending devices
   for d in /sys/bus/thunderbolt/devices/*/authorized; do
       echo 1 | sudo tee "$d" 2>/dev/null
   done
   ```

2. **Cable or connection issue:** The Apollo connects via Thunderbolt 3. Ensure the cable is fully seated. Try a different Thunderbolt port if available.

3. **Apollo not powered on:** The Apollo needs approximately 20 seconds after power-on before Thunderbolt enumeration completes. Wait and check again:
   ```bash
   sleep 20
   lspci -d 1a00:
   ```

4. **Thunderbolt security policy:** Some distributions default to `user` or `secure` Thunderbolt security. Check and adjust:
   ```bash
   cat /sys/bus/thunderbolt/devices/domain0/security
   # If "user" or "secure", you may need to use boltctl:
   boltctl list
   boltctl enroll <device-uuid>
   ```

### PCIe address changes between connections

**Note:** The PCIe bus address (e.g., `0000:07:00.0`) can change each time the Thunderbolt device is connected. This is normal for Thunderbolt devices and does not affect driver operation. Do not hardcode PCIe addresses in scripts.

---

## No Audio Output

### Step 1: Verify driver is loaded and connected

```bash
# Check driver is loaded
lsmod | grep ua_apollo

# Check dmesg for successful probe
dmesg | grep ua_apollo | grep -i "audio\|connect\|ALSA"
```

Look for:
```
ua_apollo: audio: 24 play ch, 22 rec ch, 4096 frame buf
ua_apollo: ALSA mixer: 50 controls
```

### Step 2: Verify ALSA card exists

```bash
cat /proc/asound/cards
```

You should see an entry like:
```
 1 [Apollo         ]: ua_apollo - Apollo x4
                      Apollo x4 at 0000:07:00.0
```

If the card is missing, check `dmesg` for audio initialization errors.

### Step 3: Check PipeWire sees the device

```bash
wpctl status
```

The Apollo should appear under "Audio" devices. If not:
```bash
systemctl --user restart pipewire wireplumber
```

### Step 4: Verify monitor is not muted

```bash
# Check monitor mute state
amixer -c Apollo cget name='Monitor Playback Switch'
# Unmute if needed
amixer -c Apollo cset name='Monitor Playback Switch' 1

# Check monitor volume (0 = silence, 192 = 0 dB)
amixer -c Apollo cget name='Monitor Playback Volume'
amixer -c Apollo cset name='Monitor Playback Volume' 172
```

### Step 5: Test direct ALSA playback

Bypass PipeWire to test the driver directly:

```bash
# Simple test tone
speaker-test -D hw:Apollo -c 2 -r 48000 -F S32_LE

# Play a file directly
aplay -D hw:Apollo -f S32_LE -r 48000 -c 24 /dev/zero
```

If direct ALSA playback works but PipeWire does not, see the [PipeWire configuration](/docs/configuration#pipewire-configuration) section.

---

## Audio Crackling or Dropouts

### Buffer size too small

**Symptom:** Intermittent clicks, pops, or gaps in audio.

**Fix:** Increase the PipeWire buffer size. Edit `~/.config/pipewire/pipewire.conf.d/99-apollo.conf`:

```json
context.properties = {
    default.clock.quantum = 1024
    default.clock.min-quantum = 256
    default.clock.max-quantum = 4096
}
```

Then restart PipeWire:
```bash
systemctl --user restart pipewire
```

### IRQ scheduling issues

**Symptom:** Consistent crackling under CPU load.

**Fix:** Ensure the `rtkit` daemon is running (it grants realtime scheduling to PipeWire):

```bash
systemctl status rtkit-daemon
sudo systemctl enable --now rtkit-daemon
```

For lower latency, consider adding your user to the `audio` group and configuring realtime limits in `/etc/security/limits.d/audio.conf`:

```
@audio  -  rtprio     95
@audio  -  memlock    unlimited
```

### Power management interference

**Symptom:** Audio works initially but develops glitches after idle periods, or the device disappears.

**Fix:** Disable PCIe power management for the Apollo:

```bash
# Find the Apollo's PCIe address
ADDR=$(lspci -d 1a00: | awk '{print $1}')

# Disable runtime PM
echo on | sudo tee /sys/bus/pci/devices/0000:${ADDR}/power/control
```

To make this persistent, create a udev rule:

```bash
sudo tee /etc/udev/rules.d/99-ua-apollo.rules <<EOF
ACTION=="add", SUBSYSTEM=="pci", ATTR{vendor}=="0x1a00", ATTR{power/control}="on"
EOF
```

Thunderbolt-specific power management can also cause issues:

```bash
# Disable Thunderbolt runtime PM
for d in /sys/bus/thunderbolt/devices/*/power/control; do
    echo on | sudo tee "$d" 2>/dev/null
done
```

---

## DSP Firmware Issues

### Firmware load fails

**Symptom:** `dmesg` shows firmware loading errors or DSP readback timeouts.

**Possible causes:**
- Corrupt firmware file
- Wrong firmware version

**Fix:** The firmware binary (`ua-apollo-mixer.bin`) is not included in this repository — it must be obtained from an existing macOS or Windows UAD installation. See [Building from Source — Step 4](/docs/building-from-source#step-4-install-firmware) for extraction instructions.

Verify the file has the correct magic number:
```bash
xxd /lib/firmware/ua-apollo-mixer.bin | head -1
# Should start with: 4d464155 (UAFM in little-endian)
```

### DSP not responding after load

**Symptom:** `dmesg` shows:
```
mixer DSP: readback not responding after firmware load
```

**Fix:** Power-cycle the Apollo (physically turn it off and back on), wait 20 seconds, then reload the driver:
```bash
sudo rmmod ua_apollo
# Power-cycle the Apollo hardware
sleep 20
sudo modprobe ua_apollo
```

---

## USB Apollo Issues

### Device not found after plugging in

**Symptom:** No `Apollo Solo USB` ALSA card after plugging in.

**Causes and fixes:**

1. **FX3 firmware not uploaded:** The Cypress FX3 has no onboard flash — firmware must be uploaded every power-on. Check that udev triggered the init script:
   ```bash
   dmesg | grep -i "apollo\|fx3\|2b5a"
   journalctl -u ua-usb-init --since "5 minutes ago"
   ```
   If missing, run manually:
   ```bash
   sudo bash scripts/ua-usb-init.sh
   ```

2. **Firmware files missing:** Firmware must be in `/lib/firmware/universal-audio/`. Check:
   ```bash
   ls /lib/firmware/universal-audio/ApolloSolo.bin
   ```
   If missing, obtain from UA Connect on Windows (`C:\Program Files (x86)\Universal Audio\Powered Plugins\Firmware\USB\`) and copy to `/lib/firmware/universal-audio/`.

3. **USB 2.0 port or cable:** The Apollo Solo USB requires USB 3.0 SuperSpeed. With USB 2.0, the FX3 re-enumerates but audio interfaces fail to initialize. Use a USB 3.0 port (usually blue) and a USB 3.0 cable.

4. **udev rule not installed:** Confirm the udev rule is present:
   ```bash
   cat /etc/udev/rules.d/99-apollo-usb.rules
   ```
   If missing, reinstall: `sudo bash scripts/install-usb.sh`

### No PCM streams / ALSA card shows no playback or capture

**Symptom:** The `Apollo Solo USB` card appears in `aplay -l` but shows no streams, or audio applications cannot open it.

**Cause:** The standard `snd-usb-audio` module cannot enumerate PCM streams because the device does not implement UAC 2.0 `GET_RANGE` for clock frequencies — it STALLs, which causes `snd-usb-audio` to skip stream creation.

**Fix:** The patched `snd-usb-audio` module (built by `install-usb.sh`) adds a quirk that falls back to hardcoded rates (44100, 48000, 88200, 96000, 176400, 192000 Hz) when `GET_RANGE` fails for VID `0x2B5A`. If the stock module was loaded instead:

```bash
# Check which module is active
modinfo snd-usb-audio | grep filename

# If it points to the stock module, reinstall:
sudo bash scripts/install-usb.sh
```

### USB audio dropouts or -EIO errors

**Symptom:** Isochronous transfer errors in `dmesg`, audio cuts out.

**Cause:** DSP initialization or sample rate was not set before activating audio endpoints. The device requires a specific sequence: DSP init → SET_CUR sample rate 48000 Hz → SET_INTERFACE to activate endpoints. If the DSP init script was not run or failed, audio streaming fails.

**Fix:**
```bash
sudo bash scripts/ua-usb-dsp-init.sh
# Then restart PipeWire:
systemctl --user restart pipewire wireplumber
```

### Crackling or xHCI errors on Intel systems

**Symptom:** `dmesg` shows xHCI ring buffer errors or EP6 overflow messages. Audio crackles or drops.

**Cause:** The Apollo Solo USB sends JFK notification packets on EP6 IN at approximately 2000 packets/second. Intel xHCI controllers overflow their interrupt ring buffer under this load. AMD USB controllers handle it gracefully.

**Fix:** The EP6 drain daemon (started by `tools/usb-dsp-init.py --daemon`) continuously drains EP6 before it overflows. If you are running the daemon and still see errors, ensure it started after the patched module loaded:

```bash
# Confirm the drain daemon is running
pgrep -af usb-dsp-init

# Restart if needed
sudo python3 /opt/open-apollo/tools/usb-dsp-init.py --daemon &
```

### Capture returns zeros through PipeWire

**Symptom:** Raw `arecord -D hw:USB ...` captures audio correctly, but recording via PipeWire (e.g., OBS, Ardour) produces silence.

**Cause:** Known issue as of April 2026. Suspected channel mapping or stream-open ordering interaction between PipeWire and the patched `snd-usb-audio` module when capture streams are opened after playback.

**Workaround:** Use raw ALSA capture with `arecord` until this is resolved, or stop PipeWire before recording:
```bash
systemctl --user stop pipewire pipewire-pulse wireplumber
arecord -D hw:USB -f S32_LE -r 48000 -c 10 output.wav
```

### Full init crashes at packet 28 (capture not working after usb-full-init.py)

**Symptom:** `usb-full-init.py` hangs or the device becomes unresponsive after the 28th init packet. Capture does not work even after re-plugging.

**Cause:** The 38-packet DSP init sequence was captured from Cauldron firmware v1.3 build 3. Some firmware builds place the IIR biquad SRAM region at a different address — writing to the wrong address during initialization corrupts the DSP state.

**Fix:** Unplug and re-plug the Apollo Solo USB to reset, then use the lightweight init (no capture):
```bash
sudo python3 tools/usb-dsp-init.py
```
Capture with `usb-full-init.py` will only work if your firmware matches the captured version. Check your firmware version:
```bash
python3 tools/fx3-load.py --info
```

### DSP init must run after snd-usb-audio loads (ordering issue)

**Symptom:** DSP init runs but audio endpoints remain broken. SET_INTERFACE calls after init wipe the FPGA routing table.

**Cause:** When `snd-usb-audio` opens or closes a stream, it issues SET_INTERFACE to switch alternate settings, which resets FPGA state. If the DSP init runs before the module has fully probed (and before any stream opens/closes), the routing table gets cleared.

**Fix:** The correct ordering is:
1. Load patched `snd-usb-audio` module (let it fully probe)
2. Run `tools/usb-dsp-init.py` (or wait for udev to trigger it)
3. Start PipeWire

The udev rule in `configs/udev/99-apollo-usb.rules` includes a `sleep 3` delay after module load to respect this ordering.

---

## Reading dmesg for Apollo Messages

All driver messages are prefixed with `ua_apollo`. Filter for them:

```bash
# All Apollo messages
dmesg | grep ua_apollo

# Real-time monitoring
dmesg -w | grep ua_apollo

# Just errors and warnings
dmesg | grep ua_apollo | grep -iE "error|warn|fail"
```

### Key Messages to Look For

The channel count messages below are for the Apollo x4 — other models show different counts (e.g., 8/8 for Twin X, 34/34 for x16).

| Message | Meaning |
|---------|---------|
| `Apollo x4: FPGA rev 0x...` | Device detected successfully (model name varies) |
| `audio: 24 play ch, 22 rec ch` | Channel counts detected (x4-specific) |
| `mixer DSP alive` | DSP firmware is running |
| `ACEFACE handshake OK` | Audio transport connection succeeded |
| `ALSA mixer: N controls` | ALSA controls registered |
| `DMA buffers: play=... rec=...` | DMA memory allocated |

### Enable Verbose Debug Logging

For detailed driver debugging, enable dynamic debug:

```bash
echo 'module ua_apollo +p' | sudo tee /sys/kernel/debug/dynamic_debug/control
```

This enables `dev_dbg()` messages in the driver, which include detailed register reads, DSP communication, and timing information. Disable with:

```bash
echo 'module ua_apollo -p' | sudo tee /sys/kernel/debug/dynamic_debug/control
```

---

## Collecting Debug Info for Bug Reports

When filing a bug report, include the following:

```bash
# 1. System info
uname -a
lsb_release -a 2>/dev/null || cat /etc/os-release

# 2. PCI device info
lspci -d 1a00: -vvv

# 3. Driver info
modinfo ua_apollo

# 4. ALSA info
cat /proc/asound/cards
amixer -c Apollo contents 2>/dev/null

# 5. Full dmesg (Apollo lines)
dmesg | grep ua_apollo

# 6. PipeWire status
wpctl status 2>/dev/null
pw-dump 2>/dev/null | head -100
```

Save the output and attach it to your issue on GitHub.
