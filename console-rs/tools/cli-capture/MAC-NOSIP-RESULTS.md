# SIP-disabled Apollo CLI investigation, 2026-10-13

## Results

SIP was already disabled by the user. Apollo x4 was physically present on the
Mac Studio's Thunderbolt chain, but the first boot had no loaded UA driver.
The boot log explicitly refused UAD2System approval. The user approved it and
restarted; the fresh driver reports 12.0.0 build 464 and the engine reports
DeviceOnline=true. No agent security-setting change, driver reinstall or
vendor app launch was performed.

Both text and full-packet fixtures now pass with SIP disabled. The raw
132-byte command and 304-byte response buffers were decoded offline and
matched the source-owned fixture byte-for-byte. Counts: one mock enable,
one mock send, two mock gets, four returns, zero faults. This resolves the
previous fixture's raw packet read failure under SIP, for this configuration.
The scripts now aggregate all StructMethod selectors/sizes as coverage
metadata, without collecting other selectors' payloads.

The pinned, loaded UA engine ten-second idle trace passed:

| Selector | Input bytes | Calls |
|---|---:|---:|
| 109 | 0 | 10 |
| 111 | 4 | 10 |
| 112 | 12 | 40 |
| 120 | 36 | 53 |
| 136 | 16 | 326 |
| 119 | 36 | 2608 |

Total: 3047 native calls; no 173/174/175, no trace faults. Before/after
snapshots differ in 11 values, all MeterPulse or meter levels/peaks. Thus the
observer demonstrably saw native driver traffic, while this interval supplied
no CLI command. An earlier driver-unloaded idle interval also had no CLI;
it is not evidence about normal hardware operation.

A separate ten-second idle UA Console capture attached successfully, with
zero StructMethod calls in that interval and zero faults. No GUI was operated.
A separate ten-second native CLI wrapper capture covers the six actual
framework methods, including ByHwID; ordinary driver traffic was present,
but no CLI wrapper invocation occurred. These are bounded idle observations,
not proof that CLI is never used.

One source-owned, version-guarded **consuming response query** was executed
under the full packet tracer. Scalar session-open selector 0 returned zero;
selector 175 returned zero, size 304. The entire captured response packet
was verified zero. No CLI enable or send occurred. Before/after query snapshots
again differ only in the same meter paths. This validates real native packet
capture, not firmware acknowledgement, enabled CLI, a memory reader or a
jailbreak. The known dispatcher can mask lower-level errors.

Native framework CLI entry probes are available in the engine process. No
UAD2System FBT probes were exposed in the kernel probe inventory, even with
SIP disabled. Kernel-handler tracing remains unestablished; we did not alter
boot arguments, patch the kext or attempt offset-based kernel instrumentation.

## Source identity and narrower caller question

Installed framework SHA-256 remains:
`edcce3b1012b507c8052ed62e6855a9d1cb34055015fbbd8abb95364d660bd24`.
Installed engine and retained original match SHA-256:
`8452b6066fe8c03592ba088fc08ca07333b88477a419f2fbb9b24bd108f5586a`.
Addresses below are retained arm64 disassembly, not runtime addresses.

Framework CUAD2Driver vtable begins at 0x300f8, address point 0x30108:

| Interface offset | Entry | Method |
|---|---:|---|
| +0x218 | 0x68d8 | EnableCLI |
| +0x220 | 0x693c | SendCLICommand |
| +0x228 | 0x6a00 | GetCLIResponse |
| +0x2e0 | 0x6aa8 | EnableCLIByHwID |
| +0x2e8 | 0x6b0c | SendCLICommandByHwID |
| +0x2f0 | 0x6bd0 | GetCLIResponseByHwID |

Original aligned pointers, section metadata and symbols corroborate these
slots. No direct B/BL to those entries occurs in the inspected framework.

Engine CUAD2Info constructor calls OpenUAD2Driver2("Console", 0x12, ...) at
0x1002b3b54 and stores its actual receiver at this+0x10 (0x1002b3b58).
OpenDevice repeats the call/store at 0x1002b3e84/0x1002b3e88.
UAD2PcieInterface::Initialize requests version 0xc at 0x1001b7eec and stores
its receiver at this+0x60. Framework factory 0x14bc constructs CUAD2Driver
through 0x15bc for these versions. QueryInterface 0x25d4 returns the same
object for supported IDs, rather than a demonstrated alternate CLI receiver.

The qualified receiver regions contain no corresponding immediate CLI-slot
accesses. Whole-image offset candidates resolve to unrelated mixer/image/
parameter objects; for example 0x100052a40's +0x220 receiver comes from
CMxObject::ParentOfType. Matching an offset alone is not a CLI caller.
This negative search covers straightforward immediate-offset arm64 dispatch
and nearby indirect/tail calls, not computed/obfuscated dispatch or every
other artifact. No genuine command producer or command grammar was recovered.

## Remaining investigation

Identify a real diagnostic/service caller or the firmware's command parser.
Then bind a command to this device generation and validate its semantics
before a bounded send. Capturing idle customer applications cannot manufacture
missing command vocabulary. No guessed help/dump/peek, firmware upload,
plaintext extraction, key retrieval or custom-code execution was attempted.
A device-side authentication/decryption boundary remains a separate unknown.

Raw snapshots/traces stay outside git under
~/wine-ua-work/console-port/continuation-evidence/mac-nosip-20261013/.
Only tooling and this finding ledger are committed. Native driver WIP is
untouched; Mac UA Console was observed passively, never driven.
