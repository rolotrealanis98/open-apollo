#!/bin/bash
# Exercise real grep/find over disposable files, without changing host config.
set -uo pipefail
PROJECT_DIR="$(cd "$(dirname "$0")/.." && pwd)"
# shellcheck source=scripts/dev-module.sh
source "$PROJECT_DIR/scripts/dev-module.sh"
fixture=$(mktemp -d)
trap 'rm -rf "$fixture"' EXIT
mkdir "$fixture/refs" "$fixture/modules"
fail() { echo "FAIL: $*"; exit 1; }
grep_rc=0 find_rc=0 dkms_rc=0 dkms_output=''
grep() {
    if [[ ${1:-} == -rl ]]; then
        [[ $grep_rc == 0 ]] || return "$grep_rc"
        command grep -rl "$2" "$fixture/refs"
    else
        command grep "$@"
    fi
}
find() {
    [[ $find_rc == 0 ]] || return "$find_rc"
    command find "$fixture/modules" -name 'ua_apollo*'
}
dkms() { printf '%s' "$dkms_output"; return "$dkms_rc"; }
check_autoload >/dev/null || fail 'empty fixture reported unsafe'

# More than a pipe buffer of matches: an early-exit consumer breaks pipefail.
for ((i=0; i<1600; i++)); do
    printf -v name 'ua_apollo_%0100d' "$i"
    printf 'ua_apollo\n' > "$fixture/refs/$name"
done
output=$(check_autoload) && fail 'many real grep matches reported clean'
[[ $output == *'reference found'* ]] || fail 'reference warning missing'
mv "$fixture/refs" "$fixture/modules/populated"
mkdir "$fixture/refs"
output=$(check_autoload) && fail 'many real find matches reported clean'
[[ $output == *'installed in /lib/modules'* ]] || fail 'installed-module warning missing'
mv "$fixture/modules/populated" "$fixture/saved"

for failing_command in grep find dkms; do
    grep_rc=0 find_rc=0 dkms_rc=0
    printf -v "${failing_command}_rc" '%s' 2
    output=$(check_autoload) && fail "$failing_command error reported clean"
    [[ $output == *'could not inspect'* && $output != *'no autoload references found'* ]] || fail "$failing_command error hidden"
done
grep_rc=0 find_rc=0 dkms_rc=0 dkms_output='ua_apollo/1.0, kernel, installed'
check_autoload >/dev/null && fail 'DKMS registration missed'
dkms_output='unrelated/1.0, kernel, installed'
check_autoload >/dev/null || fail 'unrelated DKMS registration rejected'
echo 'PASS: real autoload searches, large match sets, inspection failures and DKMS states'
