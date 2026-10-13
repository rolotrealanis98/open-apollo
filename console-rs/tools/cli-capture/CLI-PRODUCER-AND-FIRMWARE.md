# CLI producer and firmware boundary investigation, 2026-10-13

This pass is read-only/offline. No firmware commands, vendor app launches,
service-tool invocations, physical modifications or live Apollo writes were
performed. Read-only Mac process/listener inventory and installed hashes were
checked. Raw artifacts and analysis output remain outside git.

## Questions and result

| Question | Result | Remaining boundary |
|---|---|---|
| Do installed diagnostic helpers construct genuine CLI commands? | No qualified producer in the inspected paths | Computed dispatch/dynamic modules and other artifacts remain open |
| Can ualogger supply command text? | Its host ring stores numeric records; send logs retain a pointer, not a string copy | Kernel pointer lifetime/access is unproved; not a firmware text stream |
| Does UA Services MCP expose firmware/debug access? | Its retained 139-tool catalog describes DAW/session operations | Static catalog is not exhaustive proof about hidden/dynamic APIs |
| Can x4 update images expose a firmware command parser by carving? | Every reported candidate failed independent validation | No executable region, instruction set mapping or parser recovered |
| Does the user mapping API expose the CLI mailbox/controller? | No verified BAR0 memory-type mapping; proven mappings allocate host DMA memory | Superclass/dynamic paths remain outside coverage |
| Is custom-code execution/decryption demonstrated? | No | A readable controller image or qualified runtime loading/debug path is still needed |

## Diagnostic helper caller qualification

Artifacts below are retained under macstudio-installation/Library/Application
Support/Universal Audio. These are arm64 static addresses, not runtime pointers.
The established CUAD2Driver CLI slots are +0x218/+0x220/+0x228 and ByHwID
+0x2e0/+0x2e8/+0x2f0; an offset match requires qualified receiver provenance.

- bin/ualogger, SHA-256
  `e3955f24ea36955b243c1d0b9e307d2b41d1d9c2fdd5abbdda82d77027344777`:
  imports OpenUAD2SystemDevice, not the driver-interface factory. DriverLogReader
  constructor 0x10000ff34; read 0x1000102e8; clear 0x1000103d0; facility setters
  0x100010444/0x10001048c. No qualified CLI-slot dispatch was found.
- boot/UAD2KextLoader.app/Contents/MacOS/UAD2KextLoader, SHA-256
  `5c170876083715576e503ff601de0be2453ec58543fb520dafa3787683899569`:
  no immediate-slot dispatch candidates in the targeted scan.
- UAD Updater.app/Contents/MacOS/UAD Updater, SHA-256
  `1600bd7e743259403b4ff2d3c0e39530412009734e280c1e9c3bda4eb5f3f8b0`:
  32 offset candidates, largely image-library structures; no qualified receiver.
  Generic dlsym helper 0x10007c798 takes a symbol supplied by another caller;
  dynamic symbol construction remains unknown.
- Apollo/UA CoreAudio Control Panel.app/Contents/MacOS/UA CoreAudio Control Panel,
  SHA-256 `a8bb96c72414004582b4c8470df4b3620aa27e17fc5d1fc9f4b3f352ce7a90a7`:
  35 offset candidates were not qualified driver calls. Bind/lazy-bind tables
  contain UA logger imports at 0x100238600/0x100238608, no driver factory import.
  0x1000c7248/0x1000c7260 use a global function pointer, not an object vtable;
  0x1000c2f78 uses a callback field, not a vptr dereference. Other examples
  0x1000fc780/0x100110aa0/0x100148608 are object/library fields. Generic dlsym
  helper 0x100198200 remains a bounded dynamic-loading unknown.

These are direct-import and immediate-offset dispatch checks, not a proof
that no installed program can ever construct a CLI command.

## Host logger: positive read ABI, no copied command vocabulary

Pinned kext arm64e SHA-256:
`01d73678ba179af74ae5d1388030640ce9bb6a9cdeb1beaf2ac1cce13064bfb1`.
Source: uad-firmware/host/kext.s and original ualogger disassembly.

| Selector | Read ABI | Source |
|---|---|---|
| 146 | No input, 4-byte producer index output | DispatchLogGetIndex 0x3de44 |
| 151 | No input, 4-byte capacity-in-entries output | DispatchLogGetSize 0x3e084 |
| 149 | 16-byte input: count u32 at +8, absolute start u32 at +12; first 8 bytes ignored in this bounded handler; output 48*count bytes | DispatchLogReadEntries 0x3df44; ReadRange 0x4bc04 |

Original ualogger batches at most 84 entries/4032 bytes (Read 0x1000101fc).
ReadRange is nonconsuming; it does not advance a reader cursor. Stale/future
ranges return -104/-103. Invalid requests can themselves append diagnostics,
so a future own-source reader must bound the snapshot and stop on a race.
This pass did not execute a log query because its likely vocabulary value
was disproved by the entry storage path.

Each 48-byte entry contains messageID u32 at +0, sequence u16 at +4,
facility/severity u8 at +6/+7, uptime u64 at +8, then four u64 arguments.
CLI send 0x4f340 passes a command pointer; WriteLog 0x4c2a4 stores that numeric
argument. It does not copy ASCII into the ring. Following such a pointer is
neither an established stable string read nor a demonstrated firmware-memory
interface. No firmware-origin textual logging stream was identified.

Avoid selector 150: its retained handler returns -18 and logs. Selectors
147/152/153/154 change facility levels, clear/write logs or change DPF flags.
They are outside a minimal read-only snapshot. Selectors 148/155 query levels
and are unnecessary here. Do not execute the vendor logger assuming its
whole lifecycle matches the narrow read body.

## UA Services and its MCP bridge

Apps/UA Services.app/Contents/MacOS/UA Services, SHA-256:
`05c4cd707d8978d52caf0c791cc01feee6603b53321ef8f3ac3868ad6b2bf429`.
Its direct device path matches UAD2System at 0x10100efe4, fallback service name
at 0x10100f068, opens type-0 clients at 0x10100f14c/0x10100f1d8 and uses scalar
0/1 for session open/close. The bounded imported struct-call path contains
109 (device count, no input) at 0x10100f358 and 111 (device info, input 4)
at 0x10100f3c4, not 173–175. This is an actual device-inventory path. Absence
of direct CLI calls does not exclude dynamically loaded code.

Same app's ua-services-mcp, SHA-256:
`87e95c46a7819957253cd4ccd9a06a95e2d9d5ac5f8e94d5cd4003589d1c313c`.
It is a stdio/socket bridge to 127.0.0.1:4798 (socket 0x100001824, connect
0x100001918), with no UA factory/IOKit imports. Requests use stdin getline,
append LF and send excluding terminal NUL (0x100002060/0x10000218c).
Socket replies are NUL-delimited frames (0x1000014a4..0x1000014ac), converted
to newline output. Assuming symmetric newline TCP framing would be incorrect.

UA Services has MCP initialize/list/call response builders at 0x100f9b744,
0x100f9bb14..0x100f9bb90 and 0x100f9bd04..0x100f9bf44. Initialize advertises
protocol 2024-11-05 and server 1.0.0. The resource-load literal at 0x100f9a774
links services/tools/tools.json. Retained Contents/Resources/tools/tools.json
has SHA-256 `2d1dccba1a56757685b8c19750f4c9c846f02125ad3357643c7643918714c5c6`.
Its 139 declarations describe DAW/session operations, including mutations.
No genuine firmware CLI, DSP-memory or driver-diagnostic declaration was
identified. A session device-type setting is not raw device access.

The installed binaries' hashes match the retained pins. Read-only Mac
inventory found no running UA Services or 4798 listener, so no endpoint was
contacted or app launched. Static catalog inspection was sufficient here.
Incoming dispatch/initialized-notification prerequisites are still untraced;
initialize/tools/list is a protocol-consistent future proposal, not a fully
source-proven runtime recipe. Never use tools/call as a discovery operation.

## User mapping boundary: host DMA is not controller memory

In the same pinned kext, neither SystemUserClient nor PcieUserClient defines
clientMemoryForType; the symbol inventory supplies only the superclass import.
No vendor IOConnectMapMemory type dispatch or BAR0 access flags were established.
This is a bounded absence of a verified mapping, not proof about every macOS
mapping mechanism or an independently inspected superclass implementation.

The positively traced path is CMacDMABuffer::Initialize 0x23358 allocating
IOBufferMemoryDescriptor host memory, alignment 4096, raw options 3/0x10003
selected by (flags & 5). It explicitly zeroes the memory. MapToUser 0x236d4
calls createMappingInTask at 0x23744 for the caller task, address 0, raw mapping
options 1, offset/length 0. No observed BAR0 address enters this path.
DispatchAllocateSharedDMABuffer 0x3cf84 accepts sizes up to 16000000 bytes and
allocates through the session, taking its task from CSession+0x18.

This creates session-owned host buffers, not a view of existing controller
memory or the CLI mailbox. No allocation/map request was issued. These raw
flags were not independently assigned API enum names. The previously bounded
private DSP readback path also needs its actual runtime descriptor/lease;
a mapped host buffer alone does not provide arbitrary firmware readback.

## Firmware region scan and independent rejection

REA/Binwalk 3.1.0 performed signature scans with one worker; no extraction or
code execution. MCP lacked REA_BINWALK_COMMAND, so the existing installed tool
was supplied through a per-command environment variable to the pinned REA
6.3.0 CLI. No registration/configuration change or software installation.

| Artifact | SHA-256 | REA Evidence |
|---|---|---|
| FirmwareUpdateFreud.bin | `64eeaaaccebd85c4f82b3549bd640eb9315e824ddda102656aaa394b0a2dd34b` | `ev_d84f342d8f2e6d6f5a564679752df2e535b86f549d29554cc8660974697507dd` |
| FirmwareUpdateFreud2.bin | `19b182eba3b37ee9fbcf4c432e61ee2c60b793312adcd6a83f2ed367b1b8fe6d` | `ev_b49f676bb61ef95d5a1eea4a3c873be9ba6b1ba1a9f82afbe0aa3d6888b5fbe8` |

Freud: 2027788 bytes, three zlib candidates at offsets 125953, 477309,
1342015 (reported 10/88/23 bytes). Freud2: 4367236 bytes, seven zlib candidates
and three gpg_signed candidates. All ten zlib candidates fail Python zlib
validation as standalone reported streams: incorrect checksum or invalid
back-reference. Output was bounded to 1 MiB; no decoded bytes were published.
All three gpg_signed candidates (763791/3164264/4283071) fail GnuPG packet
validation: they are compressed-packet prefixes followed by invalid deflate,
not validated signatures. GnuPG used an isolated scratch home, no key imports
or endpoint access. Candidate fragments remain outside git, never executed.

Binwalk's raw confidence 250 is not a probability of valid firmware structure.
Reported offsets/sizes are provider candidates, not partitions, CPU addresses,
plaintext or authenticated sections. There is no validated executable region
on which to base a command-parser decompilation. This does not prove the entire
image encrypted or rule out unknown compression/container formats.

Older private FINDINGS.md makes stronger crypto claims than supported:
no repeated blocks does not by itself rule out ECB; high entropy does not
prove encryption; sibling XOR behavior alone does not identify a key or IV.
The previously inspected whole-file host transfer does not establish absence
of keys/decode paths in every other component. Cipher, signing boundary,
plaintext CPU/section layout and readable device address map remain unknown.

## Next evidence that would change the result

The [official x4 hardware manual](https://media.uaudio.com/support/manuals/hardware/Apollo%20x4%20Hardware%20Manual.pdf),
page 34, separates four SHARC DSP/DRAM blocks, an ARM processor, EEPROM and
system-control logic. It does not identify the ARM model, debug pins, boot
memory contents or where decryption occurs. The CLI mailbox cannot be assigned
to a specific processor merely from this block diagram.

A useful next artifact is a genuine service/debug command producer, a readable
controller/boot image, or a qualified device-memory read/runtime code-loading
path. Physical board/chip identification could narrow debug/boot interfaces;
no board opening or electrical probing was undertaken. Debug access and DSP
program loading must be distinguished from persistent boot-firmware replacement.
No jailbreak, plaintext extraction or key retrieval is demonstrated by this pass.

Raw results: ~/wine-ua-work/console-port/continuation-evidence/cli-producer-20261013/.
