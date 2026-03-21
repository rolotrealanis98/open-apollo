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

**Fix:**
```bash
sudo make -C driver firmware-install
# Or manually:
sudo cp ua-apollo-mixer.bin /lib/firmware/ua-apollo-mixer.bin
```

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

**Fix:** Reinstall the firmware:
```bash
sudo make -C driver firmware-install
```

The firmware file must have the correct magic number (`UAFM`) and version. Verify:
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

| Message | Meaning |
|---------|---------|
| `Apollo x4: FPGA rev 0x...` | Device detected successfully |
| `audio: 24 play ch, 22 rec ch` | Channel counts detected |
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
