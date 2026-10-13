# Mac CLI investigation: prepared capture, not a demonstrated jailbreak

## Question and success criteria

Observe a genuine caller of IOConnectCallStructMethod selectors 173–175 in
UA Mixer Engine or UA Console. Preserve command/reply text, connection,
thread, return status and the send call stack.
The full packet mode additionally aims to retain index, enable word and raw bytes.
A captured command is evidence of host behavior, not proof that firmware
accepted it. No calls during ten seconds do not establish absence of a CLI.

The framework pin is SHA-256
`edcce3b1012b507c8052ed62e6855a9d1cb34055015fbbd8abb95364d660bd24`.
Inspected arm64 wrappers: EnableCLI 0x68d8 (173, 8-byte input),
SendCLICommand 0x693c (174, 132-byte input), GetCLIResponse 0x6a00
(175, 304-byte in/out packet). Commands start at offset 4, at most 127 text
bytes; replies start at offset 4, at most 299 firmware bytes plus terminator.
`cli_calls.d` attempts complete fixed-size packet hex dumps including padding;
padding is not response content. `cli_text_calls.d` is the default text-only
fallback: bounded NUL-terminated command/reply display plus call metadata.
It omits index, enable value, padding and exact byte preservation. Control
characters can affect log display; treat all captured text as untrusted data.

## Preparation already performed, 2026-10-12

Mac Studio: macOS 27.2 build 26B5101f, arm64, SIP enabled. Read-only process
inventory found UA Mixer Engine PID 664 and UA Console PID 45109; these are
historical observations, never reusable attachment identifiers.

The fixture was compiled warning-clean on Linux and Mac. It exports an
IOConnectCallStructMethod imitation, with no IOKit imports, device access or
framework calls. It emits one mock enable, one mock send, two mock gets
(one successful, one 0xe00002c2). The trace compiled and attached to that owned
process. It observed four returns, but packet reads produced five DTrace
faults under the current configuration. A separate minimal copyin/tracemem
fixture probe faulted too. Full packet capture is therefore **not validated**.
SIP is a plausible blocker, consistent with the earlier explicit SIP
attachment refusal, but the current faults alone do not prove that disabling
SIP will resolve them. A subsequent copyinstr-only fixture check succeeded with SIP still enabled.
The complete text-only fallback then passed: counts 1/1/2/4, faults=0,
fixture-only-no-device command, fixture-response reply, expected failed-get
return, and the send stack. This validates owned-process text capture only.
No UA process was traced during this preparation.

Raw fixture logs are outside git in console-port/continuation-evidence/
mac-dtrace-preparation. Agent/security settings and UA applications were
unchanged. Temporary fixture files are on the Mac under /tmp. A persistent source copy
is staged at ~/Library/Caches/apollo-cli-investigation/prepared-20261012/;
change to that folder before running the commands below.

## Execution order

1. Start with the already validated text fallback. Re-run its fixture after
   any reboot or configuration change. The earlier real engine attachment
   failed to grab its PID, so successful owned-process tracing does not prove
   access to the vendor process. If attachment is still blocked and the user elects to change SIP for debugging, they make that change
   themselves in Recovery and reboot. Record the before/after SIP state.
   This preparation does not authorize a security-setting change. Consult
   Apple's current instructions rather than inferring Apple Silicon options:
   https://developer.apple.com/documentation/security/disabling-and-enabling-system-integrity-protection
2. Rerun the **device-free fixture first**, from a staged copy of this folder:

   ```sh
   clang -std=c11 -O0 -Wall -Wextra -Werror cli_trace_fixture.c \
     -o /tmp/apollo-cli-trace-fixture
   sudo dtrace -q -s cli_text_calls.d -c /tmp/apollo-cli-trace-fixture \
     > /tmp/apollo-cli-fixture-trace.txt 2> /tmp/apollo-cli-fixture-diagnostics.txt
   ```

   Require TRACE_READY and TRACE_END, counts enables=1 sends=1 gets=2
   returns=4 faults=0, mock send/response text, and the failed get's
   0xe00002c2 return. Verify the two text values against the fixture. To evaluate full packet
   access, rerun separately with cli_calls.d and require its hex dumps to
   match the fixture; the text result does not validate that mode. Reject attachment,
   copyin, buffer-drop or decoding failures. The fixture's synthetic name
   tests plumbing only; it does not exercise the real framework or Apollo.
3. Confirm Apollo is connected to the Mac and operational. Recompute the
   installed framework hash, identify fresh engine/Console PIDs and retain
   engine snapshots before and after using the existing read-only dump tool.
   No meter-only fluctuations should be reported as persistent state changes.
4. Start with **one ten-second idle engine capture**. It does not send device
   requests, open/close applications or operate UA Console:

   ```sh
   ps -axo pid,comm | grep 'UA Mixer Engine'
   sh run_mac_cli_capture.sh FRESH_PID /tmp/apollo-cli-engine-NEW_TIMESTAMP
   ```

   The runner defaults to text mode; append `packets` only after validating
   that fixture. It rejects unpinned frameworks/driver versions, unexpected process paths and
   existing capture folders. Logs have restrictive permissions outside git.
   It lists matching entry/return probes first, preserves diagnostics and
   rejects missing completion/faults/drops. Ten seconds is the DTrace timer;
   setup can take longer. A stuck tracer needs manual interruption, not a
   success classification. Scope is one selected process, not every UA client.
5. If that works, consider a separate ten-second idle Console capture. Any
   active GUI workflow must be named and approved separately. No guessed
   clicks, right-clicks, launch/re-sign, update, firmware upload or debug
   command injection belongs to this capture. A firmware update is not an
   acceptable way to manufacture traffic for this first experiment.
6. Review observed command/reply text (or decode raw packets in packet mode)
   offline, correlate by connection/thread and
   timing, then resolve send stack addresses against the pinned original
   binary with REA. Validate any discovered grammar statically before deciding
   on a bounded device command. Never execute strings found in a capture.
7. Restore SIP after the debugging session and verify its status. Retain raw
   logs outside git; commit only findings and source-owned tooling.

## Limitations and stop conditions

The trace covers the named public StructMethod symbol only: scalar calls,
asynchronous methods, alternative wrappers, other processes and code paths
not executed in this interval are outside coverage. Same-thread nested CLI
calls would overwrite thread-local correlation; treat unmatched/nested
records as incomplete. A successful packet getter dump requires reported size 304 and initial
capacity at least 304. The text fallback reads successful replies only when
input and output share the same pointer and input size is 304; separate
output buffers are deliberately omitted. Neither mode observes all possible
call shapes. Partial text, empty strings and faults are distinct outcomes. Native wrappers can mask lower-level errors.

Stop for trace faults/drops, changed framework version, missing probes,
unexpected persistent engine changes or device loss. No arbitrary commands,
memory modification, flash erase, protection bypass or firmware replacement
are part of the prepared experiment. Firmware grammar, memory access,
plaintext retrieval and custom-code execution remain unresolved.

## Continuation

The user disabled SIP and authorized continuation on 2026-10-13. See
[MAC-NOSIP-RESULTS.md](MAC-NOSIP-RESULTS.md) for successful raw-packet fixtures,
loaded-engine capture, the guarded response query, source identity and limits.
The earlier preparation failures above remain historical results.
