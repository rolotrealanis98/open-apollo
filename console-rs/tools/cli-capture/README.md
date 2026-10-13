# Bounded CLI response/capture investigation

These tools never guess command vocabulary, upload firmware, or drive UA
Console. The Linux banner probe explicitly enables and restores the mailbox;
the Mac response probe does not enable it. Check device state before authorized runtime use.
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
uses the inspected IOConnectCallStructMethod ABI. It prints selectors 173–175, call/return metadata
and bounded command/response packet hex dumps for at most 10 seconds. It does not
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

## Linux enable-only probe and transport failure, 2026-10-12

`linux_cli_banner_probe.py --status` reads the three mailbox control words.
`--enable-banner --output /absolute/new/file.json` performs the separately
hardware-authorized experiment: require a disabled, empty mailbox, enable for
at most 0.2 seconds, capture any unsolicited response (maximum 299 bytes), and
attempt exact restoration. It submits no command text or positive command
length. Raw output belongs outside git. A failed restoration is reported,
never treated as success. Run the eight device-free tests with:

```sh
python3 -m unittest discover -s console-rs/tools/cli-capture -p 'test_linux_cli_banner_probe.py'
```

The write sequence follows original arm64 kext EnableCLI at `0x4f124`:
C3F4 = enable, C3F8 = 0, C3FC = 0. The default Linux CLI refusal is unchanged.
This privileged diagnostic path does not prove firmware command support.
The driver and ioctl ABI must match the inspected installation before use.

The live attempt stopped at read-only preflight: the device returned ENODEV
following a fatal upstream PCIe error at 19:40:20, before any CLI writes.
Apollo-only driver rebind failed; the function's supported bus reset also
failed after its kernel timeout. The Apollo is currently unbound, with a stale
PCI listing and no usable device node; physical reconnection is pending.
No CLI enable or text writes ran. No shared-controller reset or host reboot
was performed, and the existing daemon was left running.

A bounded REA/Ghidra procedure-name search of UADMeter.arm64 (SHA-256
`7aa6bf065e991171fae864a96043bf463e5d484022205f2db0f7423e8991157e`)
found no firmware CLI command consumer. Evidence:
`ev_c5d9e3591d35cdf641c0c5d909b1694770a2a69f4d8d78fb69f32b9fa6bc3726`.
The CLI/Terminal/Debug regex also matched unrelated Client/click/debug names;
this is a bounded negative name search, not exclusion of indirect callers.
The installed Ghidra was used through a per-command environment variable;
agent configuration and original retained analysis snapshots were preserved.

## Prepared Mac tracing continuation

See [MAC-INVESTIGATION.md](MAC-INVESTIGATION.md) for the exact staged test,
source pins, stop conditions and unresolved questions. `run_mac_cli_capture.sh`
selects one fresh UA PID and a new capture directory, validates the framework
pin, and preserves diagnostics. `cli_trace_fixture.c` tests the tracer without
IOKit or a device. The SIP-enabled Mac fixture attached and observed calls,
but raw packet reads faulted. A text-only fallback subsequently passed the
same fixture with SIP enabled, including command/reply text and send stack.
This does not establish access to protected UA processes or full packet reads. Security
settings and UA apps were unchanged. The selected mode’s fixture must pass before live tracing. The runner
defaults to text mode; `packets` selects the still-unvalidated full-byte mode.
