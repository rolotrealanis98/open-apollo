#!/bin/bash
# Read-only capture predicates. Sourcing this file does not change shell options.

dtrace_allowed() {
    local sip_status="$1"

    if printf '%s\n' "$sip_status" | grep -q '^[[:space:]]*DTrace Restrictions:'; then
        printf '%s\n' "$sip_status" |
            grep -q '^[[:space:]]*DTrace Restrictions: disabled$'
        return
    fi

    printf '%s\n' "$sip_status" |
        grep -q '^System Integrity Protection status: disabled'
}
