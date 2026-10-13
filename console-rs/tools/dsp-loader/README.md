# Runtime DSP loader: verified host path, custom execution unresolved

Investigation 2026-10-13. This is runtime resource loading, not a boot-firmware
update. No program/resource registration, allocation, upload, process command
or SYNCH activation was sent to hardware. The live operations were bounded
read-only information queries through source-pinned helpers.

## What the original loader proves

Mac kext arm64e: `UAD2System.arm64e`, SHA-256
`01d73678ba179af74ae5d1388030640ce9bb6a9cdeb1beaf2ac1cce13064bfb1`.
The retained full kext and installed Mac file match SHA-256
`6119924181ced4febd32ef93c724951e4000386e52ac54ddc3564be0339b7864`.
Addresses below are retained static arm64e addresses.

| Stage | Original function | Proven boundary |
|---|---|---|
| Register | DispatchRegisterSharedResource 0x39e20; CAllocator::RegisterSharedResource 0x260d0 | Bill magic, nonzero header length/functional fields, ID/architecture identity |
| Own bytes | CSharedResource constructor 0x44bb8 | Allocates payload length +20; copies complete container; clears copied header bytes +10/+11 |
| Validate | IsValid 0x44da0 | Kind ≤4, architecture ≤2, functional count ≤total bytes/4−13; Code kind 0 requires flags 0 |
| Allocate | LoadPlugin 0x36dd4; MapResource 0x339b0 | Architecture selection, scratch and actual shared/private pool placement |
| Transfer | _transmitResource 0x33da0 | Two-word prefix plus full normalized owned Bill, with bounded descriptor chunks |
| Accept response | 0x34708–0x34780 | 16-byte reply: opcode 0x80070004, status zero, matching resource ID; fourth word uninterpreted |

Code allocation count is floor(2 × functional /3), with an evenness assertion
in the constructor. Non-Code kinds use functional directly. Do not impose
unproved divisibility by three: functional 4 converts to allocation 2.
This count is **not** the transmitted command length.

At 0x34154–0x34174, transfer framing is:

```text
word0 = (pool0 ? 0x10000 : 0x40000) | (Bill_total_bytes/4 +2)
word1 = allocated DSP address
remaining bytes = complete normalized Bill, including its header
```

Immutable flags-zero bytes are copied at 0x34230–0x34240. Chunks hold up to
1024 words. Descriptor publication, response ownership and buffer retirement
are separate requirements; neither packet construction nor a matching reply
shape grants a hardware-completion claim.

No signature-check or decrypt call was found **inside these bounded host
constructor/validation/immutable-transfer bodies**. This does not exclude
checks elsewhere, encoded body metadata, device decryption or device-side
signature enforcement. Host plugin authorization and allocation eligibility
are separate. Container bodies remain opaque; they have not been established
as decrypted SHARC instructions.

## Relocation and entry-point gap

Matching Windows12 original System SHA-256
`cb3f86aab30289f8e8519afa422fe3a6a043a9b15104d85c531fe1914936b86c`
and client SHA-256
`b9b50b49392d0972164c302ef51de4eccbf353c090007691e768451a7b79ce38`
corroborate full owned copying at 0xd4ff..0xd524 and reply validation.
Private relocation 0xcd18 builds opcode 0x150000 | (2N−1) and address pairs;
encoded resource index plus bounded word offset resolves private-state links.
That is not demonstrated rewriting of instruction bytes. The non-Code flagged
suffix transformation at 0x171f8 is not general Code/firmware decryption.

The host descriptor separately carries pluginIdentifier, Code resource IDs
and private state. The relationship from pluginIdentifier to executable entry
point/processing routine is still unresolved. A successful resource upload
would prove that transaction, not custom execution. No valid source-owned
ISA program, entry point or processing activation is supplied by this pass.

## Source-owned software check

`check_resource_contract.py` uses the already implemented source-owned pure
Windows12 model in the earlier tools-wine investigation. It loads Python
source modules from an explicit directory, constructs nonexecutable patterned
fixtures, and outputs metadata/hashes rather than commands or binary images.
Model file hashes are included in the report. The matching Mac framing and
response checks are independently anchored above; the model remains a
restricted immutable subset, not a complete Mac kernel implementation.

```sh
python3 console-rs/tools/dsp-loader/check_resource_contract.py \
  --model-dir ~/wine-ua-work/tools-wine > /tmp/dsp-resource-check.json
```

Four fixtures pass: Code, Data, Code floor conversion, and a 4096-byte Bill
crossing the descriptor-page boundary. Checks cover full-byte preservation,
normalization, transfer-vs-allocation count, page continuity, conflicting
identities, mutation, architecture selection, pool bounds/overflow, malformed
replies and wrong reply opcode/status/ID. Accepted synthetic reply shapes
explicitly retain transport/execution=false. No fixture contains a valid
SHARC program and no address is a real placement lease.

The private catalog/upload/kind3 suite passes 19 tests with no skips after
pointing UAD_NEVE1073_CORE to the hash-matching installed original. Genuine
4K, Neve and API resources were read as bytes, never executed. Relevant
retained evidence: windows-driver-wave5/allocation-report.md,
windows-driver-wave6/allocation-linkage.md,
full-plugin-support/core-bill-kind3.md and dsp-transfer-lifecycle.txt.
The older dsp_allocation_plan.py has a deliberately stricter historical
subset; its old header assumptions are not the complete current contract.
programfiles.asm is Rack program-file code, not a SHARC loader source.

## Live pool observation and architecture selection

The installed framework matches SHA-256
`edcce3b1012b507c8052ed62e6855a9d1cb34055015fbbd8abb95364d660bd24`,
and the driver is 12.0.0 build 464. The retained source-owned observer was
reused with a one-board refusal bound. It opens/closes a native host session,
queries selector109 once and selector112 for board0 DSP0..3. No allocations,
authorization refresh or parameter writes were requested. All four queries
returned status zero and 80-byte DSP information inside the 168-byte reply.

Selector112 input is 12 bytes: device index, local DSP index, want_property0.
GetDSPInfo 0x36070 reports effective allocator extents; there are **no pool
base addresses** in this verified API. The raw +0c field remains opaque.
Queries are sequential observations, not atomic allocator reservations.

| DSP | Code capacity | Code used | Pool1 used | Pool2 used | Pool3 used |
|---|---:|---:|---:|---:|---:|
| 0 | 16384 | 436 | 156 | 8 | 16384 |
| 1 | 16384 | 2546 | 1568 | 618 | 41008 |
| 2 | 16384 | 0 | 0 | 0 | 0 |
| 3 | 16384 | 0 | 0 | 0 | 0 |

Non-Code used values exclude scratch. On DSP2/3, reported capacities excluding
scratch are pool1=43320, pool2=8062, pool3=16699822; scratch reservations are
128/128/32768 respectively. These are raw allocator units, not universal byte
counts or a proven physical address map. Zero usage does not establish a
permission to place or execute code.

Architecture compatibility now has source provenance: manager constructor
0x35700–0x35774 derives state+0x210 from DSP property1, also info+0x08.
The getter at 0x12704 returns DSP+0x14; _supportedResourceArch 0x36a50 accepts
architecture0 universally, 1 when state0, and 2 when state1. All four observed
info+0x08 values are 1, so this compatibility predicate accepts 0/2. This
identifies resource selection, not a detailed ISA/program-entry specification.

Before/after engine snapshots changed only MeterPulse and two input meter
values. The existing Mac Console was not driven, plugins were not loaded,
and no program bytes were sent. Raw observations and comparisons are outside
Git in console-port/continuation-evidence/dsp-loader-20261013/.

## Actual base geometry: runtime registers and a conditional fallback

Manager Initialize 0x35ad0 requests property6, 44 bytes, via DSP virtual+0x48.
GetProperty 0x11f98 jump-table entry6 at 0x13128 resolves 0x12710. When owner
CPcieDevice+0xc54 is zero, it reads DSP-register geometry:

| Pool | Capacity offset | Base offset | Scratch offset |
|---|---:|---:|---:|
| 0 | +0x10 | +0x184 | zero |
| 2 | +0x18 | +0x18c | +0x198 |
| 1 | +0x14 | +0x188 | +0x194 |
| 3 | +0x1c | +0x190 | +0x19c |

CResourcePool::Initialize 0x33058 rounds base/scratch upward to even, capacity
downward to even, and initializes free from capacity minus scratch. Retain
raw and effective geometry separately if a valid observer becomes available.
No verified public userclient getter for the 44-byte property6 was established.
No register read, new mapping or guessed memory-type request was attempted.

The other branch supplies fixed geometry at 0x12724–0x12744. Its raw pool0/2/1/3
capacities are 0x4000/0x21ff/0xaab9/0xff55fe; bases are
0xe000/0xc5000/0x92800/0x8000000, and scratch 128/128/32768 for non-Code pools.
These are **conditional source constants, not observed placement addresses**.

The branch gate is a captured driver option: _configurePcieDevice
0x16dc8–0x16e3c obtains parameter4, passes its low32 bits to constructor
0xbfe0, which stores them at device+0xcf0 (0xc0a4). Initialize 0xe118 sets
+c54 for a nonzero captured option. Matching a capacity alone does not prove
which branch the existing device took; later allocator state can also change.

A separately bounded read-only selector167 query observed current parameter4=0:
input4 bytes parameter4; output16 bytes returned parameter at0, opaque padding,
u64 value at8. Framework GetDriverParameter 0x515c and kernel dispatcher
0x3edc4/getter0x30524/parameter4 branch0x30574 establish this ABI. It reads
manager+0x5090 without hardware refresh or a setter. The device captured the
option at construction, so its historical value remains unverified. Do not
turn the fixed table into a hardware upload profile on this evidence.

## Linux integration and next verification boundary

The separate open-apollo foundation commit199bb21 is unchanged, as are the
native worktree's existing ua_apollo.h/ua_core.c edits. Its historical
ua_dsp_load_programs path uses a guessed 0x00360000/0x80040000 sendBlock pair;
that is not the source-proven runtime Bill framing/reply contract above.
Host-only/shared-backing APIs or hardened ring preparation do not supply a
complete allocator, resource ownership, entry-point and execution lifecycle.

Before a hardware custom-code test, remaining requirements are a verified
current pool base/lease, valid target instruction/body format, program-ID to
entry-point binding, request-specific DMA/response ownership, and teardown.
Firmware acceptance/signature policy must be observed rather than inferred
from host copying. The source-owned patterned fixtures must never be uploaded.
No custom execution, plaintext/key extraction or persistent jailbreak is
claimed. This pass provides a concrete loader contract and bounded live
information, not an operational custom-program loader.
