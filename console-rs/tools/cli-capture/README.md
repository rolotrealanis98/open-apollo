# Bounded Mac CLI response/capture investigation

These tools never guess command vocabulary, enable CLI, upload firmware, or
drive UA Console. Device state must be checked before authorized runtime use.
Raw responses/traces belong outside git; they can contain UA/device data.

`mac_cli_response_probe.c` performs one **consuming** selector-175 query on
device index 0, after the source-backed scalar selector-0 session handshake.
It requires the inspected driver version 12.0.0 build 464. Verify that index 0
is the intended unit before use. The source sends neither 173 nor 174.

```sh
clang -std=c11 -Wall -Wextra -Werror mac_cli_response_probe.c \
  -framework IOKit -framework CoreFoundation -o /tmp/cli-response-probe
/tmp/cli-response-probe --check-abi                     # no device access
/tmp/cli-response-probe --capture-existing-response    # explicit authorization required
```

The latter can clear the response mailbox; it is not a passive read. A zero
return with empty data establishes neither firmware CLI enablement nor memory
read capability; the native wrappers can mask internal dispatch errors, so it
is not an MCU acknowledgement. The framework's `OpenUAD2SystemDevice` at arm64 `0x1c910`
calls scalar selector 0 after IOServiceOpen. Omitting it produced
`0xe00002c2`; the struct framing itself was correct.

`cli_calls.d` is a prepared passive template for a **selected process** that
uses the inspected IOConnectCallStructMethod ABI. It prints selectors 173–175
and bounded command/response strings for at most 10 seconds. It does not
cause the process to send a command. SIP can prevent attaching to a signed
vendor process; do not change security settings or re-sign/launch vendor apps
automatically. Failed probe attachment is not an empty successful capture.

## Actual Mac experiment, 2026-10-12

- Apollo x4 confirmed connected through CalDigit TS5 Plus; engine 12.0.0.
- Installed framework SHA-256 matches the retained source pin
  `edcce3b1012b507c8052ed62e6855a9d1cb34055015fbbd8abb95364d660bd24`.
- UA Console/engine PID tracing could not attach; kernel CLI handler tracing
  explicitly reported System Integrity Protection restrictions. SIP stayed on.
- First direct query: IOServiceOpen succeeded, but selector 175 returned
  `0xe00002c2` because the vendor session handshake was omitted.
- Corrected query: scalar selector 0 returned 0; selector 175 returned 0,
  output size 304, **zero response bytes**. The connection was closed normally.
- Before/after engine snapshots show only MeterPulse/meter fluctuations;
  no non-meter value changes. No CLI command text was sent or captured.
- `ualogger` is a logging utility using selectors 146–155, not a source of
  genuine firmware CLI commands. It was inspected, not run.

The native mailbox request path is now exercised, but a genuine text command
and firmware command grammar remain missing. A future send needs either a
documented command or a successfully captured original caller. No plaintext
firmware/key retrieval or decryption capability has been demonstrated.
