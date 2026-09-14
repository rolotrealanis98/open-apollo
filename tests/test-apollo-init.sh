#!/bin/bash
# Run the complete initializer with hardware and service boundaries substituted.
# No source slicing: the real branch ordering and failure handling execute.
set -uo pipefail
PROJECT_DIR="$(cd "$(dirname "$0")/.." && pwd)"
fixture=$(mktemp -d)
trap 'rm -rf "$fixture"' EXIT
export fixture
fail() { echo "FAIL: $*"; exit 1; }

# Only pretend the expected device and protective rule exist. All other tests
# use Bash's real builtin. Never create /dev nodes or modify system settings.
function [ {
    case "$*" in
        '-e /dev/ua_apollo0 ]'|'-f /etc/wireplumber/wireplumber.conf.d/51-ua-apollo.conf ]') return 0 ;;
        '! -e /dev/ua_apollo0 ]') return 1 ;;
        '! -f '*'/driver/ua_apollo.ko ]') return 1 ;;
    esac
    builtin [ "$@"
}
id() { echo 0; }
logname() { echo fixture-user; }
lsmod() { [[ $scenario == loadfail || $scenario == newcold ]] || echo 'ua_apollo 1 0'; }
chmod() { :; }
sleep() { :; }
hostname() { echo fixture-host; }
sudo() { echo service >> "$fixture/events"; return 93; }
wpctl() { echo service >> "$fixture/events"; return 93; }
pgrep() { echo daemon >> "$fixture/events"; return 1; }
pkill() { echo daemon >> "$fixture/events"; return 93; }
insmod() { echo load >> "$fixture/events"; [[ $scenario == newcold ]]; }
python3() {
    case "$1" in
        -)
            echo read >> "$fixture/events"
            [[ $scenario != readfail ]] || return 42
            if [[ $scenario == openfail ]]; then echo 'device_error=[Errno 13] fixture denied'; return 1; fi
            [[ $scenario != empty ]] || return 0
            local wr=0 rd=0 responds=0 pcie=1
            case "$scenario" in
                stalled|force-stalled) wr=2; rd=1 ;;
                frozen|force-frozen) wr=2; rd=2 ;;
                dead) pcie=0 ;;
                malformed) wr=oops ;;
                overflow) wr=4294967296 ;;
                unknown) rd=1 ;;
                alive) responds=1 ;;
            esac
            if command grep -q '^connect$' "$fixture/events"; then
                [[ $scenario != verifyreadfail ]] || return 42
                [[ $scenario != verifyfail ]] && responds=1
                [[ $scenario != verifymalformed ]] || wr=oops
            fi
            printf 'pcie_ok=%s\nSEQ_WR=%s\nSEQ_RD=%s\nmixer_responds=%s\n' "$pcie" "$wr" "$rd" "$responds"
            [[ $scenario != partialfail ]] || return 42
            ;;
        tools/replay-fw-blocks.py)
            echo replay >> "$fixture/events"
            if [[ $scenario == fwfail ]]; then echo 'firmware fixture failure'; return 42; fi
            echo 'Replay complete'
            ;;
        -c)
            echo connect >> "$fixture/events"
            [[ $scenario != connectfail ]] || return 42
            ;;
        *) echo unexpected-python >> "$fixture/events"; return 93 ;;
    esac
}
export -f '[' id logname lsmod chmod sleep hostname sudo wpctl pgrep pkill insmod python3
export scenario
for scenario in fwfail openfail readfail partialfail empty malformed overflow unknown stalled frozen dead connectfail verifyfail verifyreadfail verifymalformed alive cold newcold loadfail force-stalled force-frozen; do
    : > "$fixture/events"
    args=(--no-daemon)
    [[ $scenario != force-* ]] || args+=(--force)
    output=$(APOLLO_SKIP_PIPEWIRE=1 bash "$PROJECT_DIR/tools/apollo-init.sh" "${args[@]}" 2>&1)
    rc=$?
    case "$scenario" in
        alive|cold|newcold|force-stalled|force-frozen)
            [[ $rc == 0 && $output == *'Apollo initialized and ready'* ]] || fail "$scenario did not complete: $output"
            ;;
        *)
            [[ $rc != 0 && $output != *'Apollo initialized and ready'* ]] || fail "$scenario reported success: $output"
            ;;
    esac
    if [[ $scenario == openfail ]]; then
        [[ $output == *'fixture denied'* && $output != *'Cannot read DSP health data'* ]] || fail "open error text hidden: $output"
    fi
    if [[ $scenario == fwfail ]]; then
        [[ $output == *'firmware fixture failure'* && $output != *'Firmware loaded'* ]] || fail 'firmware error hidden'
        command grep -q '^connect$' "$fixture/events" && fail 'activation ran after firmware failure'
    fi
    case "$scenario" in
        readfail|openfail|partialfail|empty|malformed|overflow|unknown|stalled|frozen|dead|alive|loadfail)
            command grep -q '^replay$' "$fixture/events" && fail "$scenario incorrectly reached replay"
            ;;
    esac
    if [[ $scenario == cold || $scenario == newcold || $scenario == force-* ]]; then
        events=$(command grep -v '^load$' "$fixture/events")
        [[ $events == $'read\nreplay\nconnect\nread' ]] || fail "$scenario skipped or reordered initialization: $events"
    fi
    command grep -Eq '^(service|daemon|unexpected)' "$fixture/events" && fail "$scenario reached forbidden side effect"
done
echo 'PASS: 21 complete initializer flows, error propagation, diagnosis gates and session isolation'
