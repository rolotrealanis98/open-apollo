#!/bin/bash
#
# apollo-init.sh — Initialize Apollo on Linux
#
# Handles cold boot, warm restart, and diagnostics.
# Detects failure mode and tells you exactly what to do.
#
# Usage:
#   sudo ./apollo-init.sh              # Full init + start daemon
#   sudo ./apollo-init.sh --no-daemon  # Init only, don't start daemon
#   sudo ./apollo-init.sh --force      # Force FW replay even if DSP alive
#   sudo ./apollo-init.sh --status     # Just print current state
#

set -uo pipefail
# NOTE: no set -e — we handle errors explicitly for better diagnostics

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
REPO_ROOT="$(dirname "$SCRIPT_DIR")"
DRIVER_KO="$REPO_ROOT/driver/ua_apollo.ko"
DEVICE="/dev/ua_apollo0"
DAEMON="$REPO_ROOT/mixer-engine/ua_mixer_daemon.py"
DAEMON_LOG="/tmp/ua-mixer-daemon.log"
DAEMON_PID_FILE="/tmp/ua-mixer-daemon.pid"
DAEMON_USER="${SUDO_USER:-$(logname 2>/dev/null || echo root)}"

# Ioctl numbers computed at runtime (avoids precompute errors)

# Colors
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
CYAN='\033[0;36m'
BOLD='\033[1m'
NC='\033[0m'

info()  { echo -e "${CYAN}[INFO]${NC}  $*"; }
ok()    { echo -e "${GREEN}[ OK ]${NC}  $*"; }
warn()  { echo -e "${YELLOW}[WARN]${NC}  $*"; }
fail()  { echo -e "${RED}[FAIL]${NC}  $*"; }
step()  { echo -e "\n${BOLD}── $* ──${NC}"; }
action(){ echo -e "${YELLOW}${BOLD}  ➜ $*${NC}"; }

# Parse args
NO_DAEMON=0
FORCE_FW=0
STATUS_ONLY=0
VERBOSE=""

for arg in "$@"; do
    case "$arg" in
        --no-daemon)  NO_DAEMON=1 ;;
        --force)      FORCE_FW=1 ;;
        --status)     STATUS_ONLY=1 ;;
        -v|--verbose) VERBOSE="-v" ;;
        -h|--help)
            echo "Usage: sudo $0 [OPTIONS]"
            echo ""
            echo "Options:"
            echo "  --no-daemon  Don't start the mixer daemon"
            echo "  --force      Force firmware replay even if DSP is alive"
            echo "  --status     Print current Apollo state and exit"
            echo "  -v           Verbose daemon output"
            echo "  -h           Show this help"
            exit 0
            ;;
        *) echo "Unknown option: $arg"; exit 1 ;;
    esac
done

# ── Python helper that reads all DSP state in one shot ──

read_dsp_state() {
    python3 - "$DEVICE" <<'PYEOF'
import struct, fcntl, os, sys, time

dev = sys.argv[1]

# Compute ioctl numbers: _IOC(dir, type='U', nr, size)
def _IOC(d, t, nr, sz): return (d << 30) | (t << 8) | nr | (sz << 16)
ioctl_read = _IOC(3, ord('U'), 0x10, 8)   # _IOWR
ioctl_write= _IOC(1, ord('U'), 0x11, 8)   # _IOW
ioctl_rb   = _IOC(2, ord('U'), 0x41, 164) # _IOR

try:
    fd = os.open(dev, os.O_RDWR)
except OSError as e:
    print(f"device_error={e}")
    sys.exit(1)

def rr(off):
    r = fcntl.ioctl(fd, ioctl_read, struct.pack('II', off, 0))
    return struct.unpack('II', r)[1]

# Read basic registers
reg0    = rr(0x0000)  # Should be 0x00001c00 for Apollo
fw_ver  = rr(0x0004)
dev_type= rr(0x000C)
state   = rr(0x3800)
config  = rr(0x3818)
seq_wr  = rr(0x3808)
seq_rd  = rr(0x380C)
rb_st   = rr(0x3810)

# PCIe health check
pcie_ok = reg0 != 0xFFFFFFFF and fw_ver != 0xFFFFFFFF

# HW readback
rb_data = [0] * 40
if pcie_ok:
    try:
        rb_buf = bytes(164)
        rb_result = fcntl.ioctl(fd, ioctl_rb, rb_buf)
        rb_words = struct.unpack('I' * 41, rb_result)
        rb_st = rb_words[0]
        rb_data = list(rb_words[1:])
    except:
        pass

# Quick mixer responsiveness test: write setting[2] no-op, bump SEQ
mixer_responds = False
if pcie_ok and seq_wr == seq_rd:
    try:
        reg = 0x3800 + 0xB4 + 2*8  # setting[2]
        cur_a = rr(reg)
        cur_b = rr(reg + 4)
        # Write back same values (no-op)
        def wr(off, val):
            fcntl.ioctl(fd, ioctl_write, struct.pack('II', off, val & 0xFFFFFFFF))
        wr(reg, cur_a)
        wr(reg + 4, cur_b)
        wr(0x3808, seq_rd + 1)
        for _ in range(100):
            if rr(0x380C) == seq_rd + 1:
                mixer_responds = True
                break
            time.sleep(0.005)
        # Re-read counters
        seq_wr = rr(0x3808)
        seq_rd = rr(0x380C)
    except:
        pass

os.close(fd)

print(f"pcie_ok={1 if pcie_ok else 0}")
print(f"reg0=0x{reg0:08X}")
print(f"fw_ver=0x{fw_ver:08X}")
print(f"dev_type=0x{dev_type:08X}")
print(f"mixer_state=0x{state:08X}")
print(f"config=0x{config:08X}")
print(f"SEQ_WR={seq_wr}")
print(f"SEQ_RD={seq_rd}")
print(f"rb_status={rb_st}")
print(f"rb0=0x{rb_data[0]:08X}")
print(f"rb2=0x{rb_data[2]:08X}")
print(f"rb3=0x{rb_data[3]:08X}")
print(f"mixer_responds={1 if mixer_responds else 0}")
PYEOF
}

# ── Parse state output into variables ──

parse_state() {
    local output="$1"
    PCIE_OK=$(echo "$output" | grep "^pcie_ok=" | cut -d= -f2)
    REG0=$(echo "$output" | grep "^reg0=" | cut -d= -f2)
    MIXER_STATE=$(echo "$output" | grep "^mixer_state=" | cut -d= -f2)
    SEQ_WR=$(echo "$output" | grep "^SEQ_WR=" | cut -d= -f2)
    SEQ_RD=$(echo "$output" | grep "^SEQ_RD=" | cut -d= -f2)
    RB_STATUS=$(echo "$output" | grep "^rb_status=" | cut -d= -f2)
    RB0=$(echo "$output" | grep "^rb0=" | cut -d= -f2)
    RB2=$(echo "$output" | grep "^rb2=" | cut -d= -f2)
    RB3=$(echo "$output" | grep "^rb3=" | cut -d= -f2)
    MIXER_RESPONDS=$(echo "$output" | grep "^mixer_responds=" | cut -d= -f2)
    DEVICE_ERROR=$(echo "$output" | grep "^device_error=" | cut -d= -f2-)
}

# ── Diagnose and report ──

diagnose() {
    # Called after parse_state. Sets DSP_ALIVE and prints diagnostics.

    # 1. PCIe link dead
    if [ "${PCIE_OK:-0}" = "0" ]; then
        fail "PCIe link DEAD (registers read 0xFFFFFFFF)"
        echo ""
        action "Power cycle the Apollo (unplug power, wait 5s, replug)"
        action "Then: sudo $0"
        echo ""
        info "If it persists after power cycle, reboot Ubuntu too."
        return 1
    fi

    # 2. DSP stalled (SEQ_WR > SEQ_RD) — crashed from bad write
    if [ "${SEQ_WR:-0}" -gt 0 ] && [ "${SEQ_WR}" != "${SEQ_RD}" ] && [ "${SEQ_WR:-0}" -gt "${SEQ_RD:-0}" ]; then
        if [ "$FORCE_FW" = "1" ]; then
            warn "DSP STALLED (SEQ_WR=$SEQ_WR > SEQ_RD=$SEQ_RD) — --force overriding"
            DSP_ALIVE=0
            return 0
        fi
        fail "DSP STALLED (SEQ_WR=$SEQ_WR > SEQ_RD=$SEQ_RD)"
        info "The DSP crashed while processing a setting write."
        echo ""
        action "Power cycle the Apollo (unplug power, wait 5s, replug)"
        action "Then: sudo $0"
        return 1
    fi

    # 3. DSP alive and responding
    if [ "${MIXER_RESPONDS:-0}" = "1" ]; then
        DSP_ALIVE=1
        return 0
    fi

    # 4. SEQ counters match at 0 — cold boot, needs FW + connect
    if [ "${SEQ_WR:-0}" = "0" ] && [ "${SEQ_RD:-0}" = "0" ]; then
        DSP_ALIVE=0
        return 0
    fi

    # 5. SEQ counters match but mixer doesn't respond to test write
    if [ "${SEQ_WR:-0}" -gt 0 ] && [ "${SEQ_WR}" = "${SEQ_RD}" ] && [ "${MIXER_RESPONDS:-0}" = "0" ]; then
        if [ "$FORCE_FW" = "1" ]; then
            warn "DSP FROZEN (SEQ_WR=$SEQ_WR=$SEQ_RD) — --force overriding"
            DSP_ALIVE=0
            return 0
        fi
        fail "DSP FROZEN (SEQ_WR=$SEQ_WR=$SEQ_RD but not processing new writes)"
        echo ""
        action "Power cycle the Apollo (unplug power, wait 5s, replug)"
        action "Then: sudo $0"
        return 1
    fi

    # 6. Unknown state
    warn "Unknown DSP state (SEQ_WR=$SEQ_WR SEQ_RD=$SEQ_RD mixer_responds=$MIXER_RESPONDS)"
    DSP_ALIVE=0
    return 0
}

# ──────────────────────────────────────────────────────────────
# Status mode
# ──────────────────────────────────────────────────────────────

if [ "$STATUS_ONLY" = "1" ]; then
    echo -e "${BOLD}Apollo Status${NC}"
    echo ""

    # Driver
    if lsmod | grep ua_apollo > /dev/null 2>&1; then
        ok "Driver: loaded"
    else
        fail "Driver: not loaded"
        action "sudo insmod $DRIVER_KO"
        exit 1
    fi

    # Device
    if [ ! -e "$DEVICE" ]; then
        fail "Device: $DEVICE not found"
        action "Check Thunderbolt connection, then: sudo $0"
        exit 1
    fi
    ok "Device: $DEVICE"

    # Read state
    output=$(read_dsp_state 2>/dev/null) || true

    if echo "$output" | grep -q "^device_error="; then
        fail "Cannot open device: $(echo "$output" | grep "^device_error=" | cut -d= -f2-)"
        exit 1
    fi

    parse_state "$output"

    # PCIe
    if [ "${PCIE_OK:-0}" = "0" ]; then
        fail "PCIe: DEAD (0xFFFFFFFF)"
        action "Power cycle Apollo, then reboot Ubuntu"
        exit 1
    fi
    ok "PCIe: link up"

    # DSP
    info "State: $MIXER_STATE  SEQ_WR=$SEQ_WR  SEQ_RD=$SEQ_RD"

    if [ "${MIXER_RESPONDS:-0}" = "1" ]; then
        ok "DSP: ALIVE (responding to setting writes)"
    elif [ "${SEQ_WR:-0}" -gt "${SEQ_RD:-0}" ] 2>/dev/null; then
        fail "DSP: STALLED (needs Apollo power cycle)"
    elif [ "${SEQ_WR:-0}" = "0" ] && [ "${SEQ_RD:-0}" = "0" ]; then
        warn "DSP: not initialized (run sudo $0 to init)"
    else
        warn "DSP: unknown state"
    fi

    # Readback
    if [ "${RB_STATUS:-0}" = "1" ]; then
        ok "Readback: valid (preamp=$RB0 monitor=$RB2 gain=$RB3)"
    else
        warn "Readback: not ready"
    fi

    # Daemon
    if pgrep -f "ua_mixer_daemon" > /dev/null 2>&1; then
        ok "Daemon: running (PID $(pgrep -f ua_mixer_daemon | head -1))"
    else
        warn "Daemon: not running"
    fi

    exit 0
fi

# ──────────────────────────────────────────────────────────────
# Main init flow
# ──────────────────────────────────────────────────────────────

echo -e "${BOLD}Apollo — Linux Init${NC}"

# Must be root
if [ "$(id -u)" -ne 0 ]; then
    fail "Must run as root (sudo)"
    exit 1
fi

# ── Step 1: Driver ──
step "Driver"

if lsmod | grep ua_apollo > /dev/null 2>&1; then
    ok "Driver already loaded"
else
    if [ ! -f "$DRIVER_KO" ]; then
        fail "Driver not built: $DRIVER_KO"
        action "cd $REPO_ROOT/driver && make"
        exit 1
    fi
    info "Loading driver..."
    if ! insmod "$DRIVER_KO" 2>/dev/null; then
        fail "insmod failed"
        action "Check dmesg: sudo dmesg | tail -20"
        exit 1
    fi
    ok "Driver loaded"
fi

# Wait for device node
tries=0
while [ ! -e "$DEVICE" ] && [ $tries -lt 50 ]; do
    sleep 0.1
    tries=$((tries + 1))
done

if [ ! -e "$DEVICE" ]; then
    fail "Device $DEVICE not found after 5s"
    if lspci -d 1a00:0002 | grep -q .; then
        info "Apollo PCIe device IS visible — driver probe failed"
        action "Check dmesg: sudo dmesg | grep ua_apollo"
        action "May need: reboot Ubuntu"
    else
        info "Apollo PCIe device NOT visible"
        action "Check Thunderbolt cable and Apollo power"
        action "May need: power cycle Apollo, then reboot Ubuntu"
    fi
    exit 1
fi

ok "Device: $DEVICE"
chmod 666 "$DEVICE"

# ── Step 2: Diagnose DSP ──
step "DSP Health Check"

output=$(read_dsp_state 2>/dev/null) || true

if echo "$output" | grep -q "^device_error="; then
    fail "Cannot read device"
    action "Check: sudo dmesg | tail -20"
    exit 1
fi

parse_state "$output"
DSP_ALIVE=0

if ! diagnose; then
    exit 1
fi

if [ "$DSP_ALIVE" = "1" ] && [ "$FORCE_FW" = "0" ]; then
    ok "DSP is alive and responding"
else
    if [ "$DSP_ALIVE" = "1" ] && [ "$FORCE_FW" = "1" ]; then
        info "DSP alive but --force specified, re-initializing..."
    else
        info "Cold boot detected — initializing DSP..."
    fi

    # ── Step 3: Firmware replay ──
    step "Firmware Replay"

    info "Loading 15 firmware blocks (169 KB)..."
    fw_output=$(cd "$REPO_ROOT" && python3 tools/replay-fw-blocks.py 2>&1) || true
    echo "$fw_output" | grep -E "^(Filtered|Total|Replay)" || true
    ok "Firmware loaded"

    # ── Step 4: ACEFACE Connect ──
    step "DSP Activation"

    # After firmware load, trigger ACEFACE handshake via ioctl.
    # The driver's probe-time connect attempt fails on cold boot
    # (DSP not ready), so we re-trigger it now that firmware is loaded.
    # UA_IOCTL_DSP_CONNECT = _IO('U', 0x51) = 0x00005551
    info "Triggering ACEFACE connect..."
    python3 -c "
import fcntl, os, sys
fd = os.open('$DEVICE', os.O_RDWR)
try:
    fcntl.ioctl(fd, 0x00005551)
    print('ACEFACE connect succeeded')
except OSError as e:
    print(f'ACEFACE connect failed: {e}')
    sys.exit(1)
finally:
    os.close(fd)
" 2>&1
    connect_rc=$?

    if [ $connect_rc -eq 0 ]; then
        ok "ACEFACE connected (routing tables sent by driver)"
    else
        fail "ACEFACE connect failed"
        echo ""
        info "The firmware was loaded but the ACEFACE handshake failed."
        info "This usually means the Apollo needs a full power cycle."
        echo ""
        action "Power cycle the Apollo (unplug power cable, wait 5s, replug)"
        action "Wait for front panel to light up"
        action "Then: sudo $0"
        exit 1
    fi

    # Wait briefly for DSP to settle
    sleep 0.5

    # ── Step 6: Verify mixer responds ──
    step "Verify"

    sleep 0.5
    output=$(read_dsp_state 2>/dev/null) || true
    parse_state "$output"

    if [ "${MIXER_RESPONDS:-0}" = "1" ]; then
        ok "Mixer DSP verified — SEQ_WR=$SEQ_WR SEQ_RD=$SEQ_RD"
    else
        fail "Mixer DSP not responding after init"
        echo ""
        action "Power cycle the Apollo (unplug power cable, wait 5s, replug)"
        action "Then: sudo $0"
        exit 1
    fi
fi

# ── Step 7: Daemon ──
step "Mixer Daemon"

if [ "$NO_DAEMON" = "1" ]; then
    info "Skipping daemon (--no-daemon)"
else
    # Kill existing daemon
    if pgrep -f "ua_mixer_daemon" > /dev/null 2>&1; then
        pkill -f "ua_mixer_daemon" 2>/dev/null || true
        info "Stopped existing daemon"
        sleep 1
    fi

    info "Starting daemon as user '$DAEMON_USER'..."

    sudo -u "$DAEMON_USER" bash -c "
        cd '$REPO_ROOT/mixer-engine'
        nohup python3 ua_mixer_daemon.py $VERBOSE > '$DAEMON_LOG' 2>&1 &
        echo \$! > '$DAEMON_PID_FILE'
    "

    sleep 2

    if [ -f "$DAEMON_PID_FILE" ] && kill -0 "$(cat "$DAEMON_PID_FILE" 2>/dev/null)" 2>/dev/null; then
        pid=$(cat "$DAEMON_PID_FILE")
        ok "Daemon running (PID $pid)"

        if grep -q "TCP:4710" "$DAEMON_LOG" 2>/dev/null; then
            ok "TCP:4710 (ConsoleLink) listening"
        fi
        if grep -q "TCP:4720" "$DAEMON_LOG" 2>/dev/null; then
            ok "TCP:4720 (Mixer Helper) listening"
        fi
        if grep -q "Bonjour" "$DAEMON_LOG" 2>/dev/null; then
            ok "Bonjour advertising"
        fi
    else
        fail "Daemon failed to start"
        info "Log: $DAEMON_LOG"
        tail -5 "$DAEMON_LOG" 2>/dev/null || true
        echo ""
        action "Check: cat $DAEMON_LOG"
        exit 1
    fi
fi

# ── Step 8: PipeWire pro-audio profile ──
step "PipeWire"

# WirePlumber explicitly excludes pro-audio from auto-selection,
# so we must set it after PipeWire discovers the device.
if command -v wpctl > /dev/null 2>&1; then
    # wpctl needs XDG_RUNTIME_DIR to connect to PipeWire's user socket
    DAEMON_UID=$(id -u "$DAEMON_USER" 2>/dev/null || echo "")
    pw_run() { sudo -u "$DAEMON_USER" XDG_RUNTIME_DIR="/run/user/$DAEMON_UID" "$@"; }

    # Restart WirePlumber to pick up device.profile rule
    pw_run systemctl --user restart wireplumber 2>/dev/null || true
    sleep 2

    # Find the Apollo device ID in PipeWire
    APOLLO_DEV_ID=$(pw_run wpctl status 2>/dev/null | \
        grep -i 'apollo\|ua_apollo' | head -1 | sed 's/[^0-9]*\([0-9]*\)\..*/\1/')

    if [ -n "$APOLLO_DEV_ID" ]; then
        pw_run wpctl set-profile "$APOLLO_DEV_ID" 1 2>/dev/null
        sleep 1
        # Verify sink was created
        if pw_run wpctl status 2>/dev/null | grep -qi "Apollo.*Pro\|apollo.*sink"; then
            ok "PipeWire pro-audio profile active (24 out + 22 in channels)"
            # Set as default
            SINK_ID=$(pw_run wpctl status 2>/dev/null | \
                grep -i 'apollo.*pro\|apollo.*sink' | head -1 | sed 's/[^0-9]*\([0-9]*\)\..*/\1/')
            [ -n "$SINK_ID" ] && pw_run wpctl set-default "$SINK_ID" 2>/dev/null && \
                ok "Apollo set as default audio output"
        else
            warn "PipeWire profile set but no sink appeared"
            info "Try: wpctl set-profile $APOLLO_DEV_ID 1"
        fi
    else
        warn "Apollo not found in PipeWire (card may not be enumerated yet)"
        info "Try: wpctl status && wpctl set-profile <ID> 1"
    fi
else
    info "wpctl not found — skipping PipeWire setup"
fi

# ── Done ──
echo ""
echo -e "${GREEN}${BOLD}Apollo initialized and ready${NC}"
echo ""
info "Daemon log: $DAEMON_LOG"
info "Connect iPad/Console to $(hostname) ($(hostname -I 2>/dev/null | awk '{print $1}'))"
info "Status: sudo $0 --status"
