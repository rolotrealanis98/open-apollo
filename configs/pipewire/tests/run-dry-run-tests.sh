#!/bin/bash
# Compare setup-apollo-io.sh --dry-run -v --virtual (and --extra, except for the x4 captures) against the saved
# output for every io_map fixture in this directory.
#
#   configs/pipewire/tests/run-dry-run-tests.sh            check
#   configs/pipewire/tests/run-dry-run-tests.sh --update   rewrite the .expected files
#
# x4-io_map-*.txt are real captures from an Apollo x4; their .expected files
# were produced by the script as it was before it learned other models, so a
# passing run shows the x4 result did not change.  The other fixtures are
# synthesized from devices/*.json by tools/gen-io-map-fixture.py.
# (The default run without --extra/--virtual is a subset of this output;
# x4-virtual-auto.expected checks the default Virtual choice.)

set -u
unset CDPATH
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ROOT="$(cd "$HERE/../../.." && pwd)"
cd "$ROOT" || exit 1

update=0
[ "${1:-}" = "--update" ] && update=1
fail=0
for f in configs/pipewire/tests/*-io_map-*.txt; do
    exp="${f%.txt}.expected"
    case "$f" in
        *x4-io_map-48k.txt|*x4-io_map-192k.txt) extra="" ;;   # captures: the pre-multi-model set
        *) extra="--extra" ;;
    esac
    out="$(bash configs/pipewire/setup-apollo-io.sh --dry-run -v $extra --virtual --io-map "$f" 2>&1)"
    if [ "$update" = 1 ]; then
        printf '%s\n' "$out" > "$exp"
        echo "wrote $exp"
    elif [ "$out" = "$(cat "$exp" 2>/dev/null)" ]; then
        echo "ok   $f"
    else
        echo "FAIL $f"
        diff <(printf '%s\n' "$out") "$exp" | head -20
        fail=1
    fi
done

# The default Virtual choice: a pair gets its loopbacks only when the driver's
# dsp_state shows a live Input Strip on both inputs (here 1+2; 3+4 is not,
# Virtual 4's strip is still queued)
f=configs/pipewire/tests/x4-io_map-48k.txt
exp=configs/pipewire/tests/x4-virtual-auto.expected
out="$(bash configs/pipewire/setup-apollo-io.sh --dry-run --io-map "$f" \
    --dsp-state configs/pipewire/tests/x4-dsp_state-virtual.txt 2>&1)"
if [ "$update" = 1 ]; then
    printf '%s\n' "$out" > "$exp"
    echo "wrote $exp"
elif [ "$out" = "$(cat "$exp" 2>/dev/null)" ]; then
    echo "ok   $exp"
else
    echo "FAIL $exp"
    diff <(printf '%s\n' "$out") "$exp" | head -20
    fail=1
fi
exit $fail
