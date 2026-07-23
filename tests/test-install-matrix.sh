#!/usr/bin/env bash
# test-install-matrix.sh — Test install script across all supported distros
# Usage: ./tests/test-install-matrix.sh [distro1 distro2 ...]
# Without args, tests all distros.
# Requires: Docker, bash 4+ (macOS: brew install bash)

set -uo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
REPO_DIR="$(cd "$SCRIPT_DIR/.." && pwd)"

DISTROS=(ubuntu ubuntu-22.04 ubuntu-20.04 fedora fedora-41 fedora-40 debian debian-bookworm debian-bullseye arch opensuse)
if [ $# -gt 0 ]; then
    DISTROS=("$@")
fi

declare -A RESULTS
PASS=0
FAIL=0

echo "============================================"
echo " Open Apollo — Install Matrix Test"
echo "============================================"
echo ""

for distro in "${DISTROS[@]}"; do
    DOCKERFILE="$SCRIPT_DIR/docker/Dockerfile.$distro"
    IMAGE="open-apollo-test:$distro"

    if [ ! -f "$DOCKERFILE" ]; then
        echo "[$distro] SKIP — no Dockerfile"
        RESULTS[$distro]="SKIP"
        continue
    fi

    echo "[$distro] Building image..."
    BUILD_LOG=$(mktemp)
    if ! docker build -t "$IMAGE" -f "$DOCKERFILE" "$REPO_DIR" >"$BUILD_LOG" 2>&1; then
        echo "[$distro] FAIL — Docker build failed"
        tail -20 "$BUILD_LOG"
        rm -f "$BUILD_LOG"
        RESULTS[$distro]="FAIL (build image)"
        FAIL=$((FAIL + 1))
        continue
    fi
    rm -f "$BUILD_LOG"

    echo "[$distro] Running install + verification..."
    OUTPUT=$(docker run --rm "$IMAGE" bash -c "
        TEST_BUILD=1 bash scripts/install.sh --skip-init --no-dkms 2>&1
        RC=\$?
        echo '---VERIFY---'
        echo \"RC=\$RC\"
        echo \"DISTRO=\$(. /etc/os-release && echo \$ID)\"
        ls /etc/wireplumber/wireplumber.conf.d/51-ua-apollo.conf 2>/dev/null && echo 'WP_CONF=0.5'
        ls /etc/wireplumber/main.lua.d/51-ua-apollo.lua 2>/dev/null && echo 'WP_CONF=0.4'
        ls /usr/share/alsa/ucm2/ua_apollo/ua_apollo.conf 2>/dev/null && echo 'UCM2=yes' || echo 'UCM2=no'
        ls /usr/local/bin/apollo-setup-io 2>/dev/null && echo 'SETUP_IO=yes' || echo 'SETUP_IO=no'
        find /opt/open-apollo/driver -name 'ua_apollo.ko' 2>/dev/null | head -1 | grep -q . && echo 'KO_BUILT=yes' || echo 'KO_BUILT=no'
    " 2>&1)

    VERIFY=$(echo "$OUTPUT" | sed -n '/---VERIFY---/,$p')

    # Extract values portably (no grep -P needed)
    extract_val() { echo "$VERIFY" | grep "^$1=" | head -1 | cut -d= -f2; }
    INSTALL_RC=$(extract_val RC)
    INSTALL_RC=${INSTALL_RC:-999}
    WP_CONF=$(extract_val WP_CONF)
    WP_CONF=${WP_CONF:-none}
    UCM2=$(extract_val UCM2)
    UCM2=${UCM2:-?}
    SETUP_IO=$(extract_val SETUP_IO)
    SETUP_IO=${SETUP_IO:-?}
    KO_BUILT=$(extract_val KO_BUILT)
    KO_BUILT=${KO_BUILT:-?}

    if [ "$INSTALL_RC" = "0" ] || echo "$OUTPUT" | grep -q "Installation complete"; then
        echo "[$distro] PASS  (wp=$WP_CONF ucm2=$UCM2 setup_io=$SETUP_IO ko=$KO_BUILT)"
        RESULTS[$distro]="PASS (wp=$WP_CONF ucm2=$UCM2 setup_io=$SETUP_IO ko=$KO_BUILT)"
        PASS=$((PASS + 1))
    else
        echo "[$distro] FAIL  (exit=$INSTALL_RC)"
        echo "$OUTPUT" | grep -v -F -e '---VERIFY---' | tail -15
        RESULTS[$distro]="FAIL (exit=$INSTALL_RC)"
        FAIL=$((FAIL + 1))
    fi
    echo ""
done

# Summary
echo "============================================"
echo " RESULTS"
echo "============================================"
for distro in "${DISTROS[@]}"; do
    printf "  %-12s %s\n" "$distro" "${RESULTS[$distro]:-SKIP}"
done
echo ""
echo "Passed: $PASS  Failed: $FAIL"
echo "============================================"

[ $FAIL -eq 0 ] && exit 0 || exit 1
