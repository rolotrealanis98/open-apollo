# Apollo-assisted firmware recovery: evidence and limits

Read-only investigation, 2026-10-12. No CLI enable/send/read calls, raw register
access, firmware upload, key extraction, native plug-in load or vendor app
execution was performed. Existing driver edits and assets were preserved.

## Answer

An Apollo could in principle expose plaintext after internally decoding a
firmware image, without exporting its key. That requires a proven readable
plaintext region or an interface returning decrypted bytes. Neither has been
established here. The available CLI selectors prove a text mailbox, not a
decryption service, arbitrary memory reader or instruction-memory aperture.

The earlier statement that the device cannot be a decryption oracle is too
absolute. Conversely, the claim that its CLI can dump decrypted code is also
unproved. An updater success/failure reply is not itself a plaintext oracle.

## Pinned CLI evidence

Local framework: `wine-ua-work/macstudio-installation/Library/Frameworks/`
`UAD-2 Plugin Support.framework/Versions/A/UAD-2 Plugin Support`, whole-fat
SHA-256 `edcce3b1012b507c8052ed62e6855a9d1cb34055015fbbd8abb95364d660bd24`.
The following addresses are its **arm64 slice**, not the inventory's x86 slice.

| Selector | Function/address | Observed payload |
|---|---|---|
| 173 | EnableCLI, `0x68d8` | 8-byte input: device index and enable integer; 4-byte status output |
| 174 | SendCLICommand, `0x693c` | 132-byte input: device index and 128-byte text buffer, forcibly NUL terminated |
| 175 | GetCLIResponse, `0x6a00` | 304-byte index/response structure; framework copies only 128 response bytes to caller |

Kext arm64e: `wine-ua-work/uad-firmware/host/UAD2System.arm64e`, SHA-256
`01d73678ba179af74ae5d1388030640ce9bb6a9cdeb1beaf2ac1cce13064bfb1`.
Retained disassembly: `uad-firmware/host/kext.s`.

- UserClient wrappers `0xb2e8/0xb334/0xb380` require a session/messenger.
- Dispatch `0x3f5a8/0x3f6f4/0x3f83c` checks manager state and buffer sizes;
  DeviceManager routes by device index through an audio-extension object.
- Enable at `0x4f124` writes BAR0 offsets `0xc3f4`, `0xc3f8`, `0xc3fc`.
- Send at `0x4f1d0` waits for the command-length mailbox to clear, uses
  `strlen`, copies text to `0xc400`, then publishes its length at `0xc3f8`.
- Receive at `0x4f46c` reads up to 299 bytes from `0xc480`, terminates the
  string and **writes zero to response length `0xc3fc`**. It is not passive.

Host wrappers do not expose the device command parser. No proven `help`,
`peek`, memory-dump command, privilege mode or plaintext address map was found
in the bounded retained host/capture search. The ARM/FPGA endpoint relationship
also remains unproved; mailbox offsets alone do not locate the decryptor.

## Native CLI mismatch and existing guard

`open-apollo-plugin-native/driver/ua_core.c`, `ua_cli_send_locked`, explicitly
returns `-ENOSYS`; comments record Apollo x4 ARM/front-panel freezes under the
attempted CLI path. This does not prove correctly framed text always freezes.
The old Python helper's binary preamp packets and high-bit completion rule do
not match the traced vendor text path and should not be used for this probe.

The generic `UA_IOCTL_WRITE_REG` requires `CAP_SYS_RAWIO` but does not blacklist
the CLI mailbox. Its privileged Python caller can therefore bypass the CLI
refusal by writing those offsets directly. Do not treat that escape as a
validated CLI implementation or remove the refusal to conduct this experiment.

## Firmware and DSP distinctions

Source-pinned `analysis/licensing-contract/firmware-container-boundary.md`
establishes whole-file transfer: Meter `LoadBinFile` at `0x10013eba4` copies
the full FBUT/GBUT/HBUT object into DMA; kext `LoadFirmware` at `0x134e0`
sends `0x120000`, expected reply `0x80040000`, with a 150000 ms timeout.
These bounded host bodies do not decode the image or return plaintext.

High entropy and sibling-file differences support an encryption hypothesis;
they do not identify the cipher, IV/key derivation, authentication, executable
sections or decryption processor. The older `uad-firmware/FINDINGS.md` claim
that the key exists in no host binary exceeds the inspected-path evidence.
No actual key location or absence across all host components was proved.

Boot MCU memory, FPGA configuration, SHARC program memory and host BAR/DMA
apertures are different targets. Internal execution does not establish host
readback access to any of them. The retained `0xc0004` DSP readback path is
bound to descriptor-owned private runtime leases; it is not demonstrated
arbitrary boot/flash/instruction-memory access (`windows-driver-wave40/`
`generic-audio-review.md`).

Plug-in Bill resources are a separate investigation from update containers.
Installed core resources already provide accessible source bytes, but neither
their executability nor device-side transformation should be assumed from
the container label. The Linux "mixer firmware" capture is explicitly a set
of Bill resource streams, not a recovered DSP boot image (`ua_dsp.c`,
`ua_dsp_load_mixer_blocks`).

## Next evidence needed

1. Identify a genuine caller/capture supplying a textual CLI command and bind
   it to this artifact/device generation. Build a mock of that exact mailbox
   protocol before any real experiment.
2. Establish whether a read command exists, its permitted address space and
   production/debug authorization. No guessed command/address probes.
3. Independently identify the firmware section/decode boundary and bind a
   readable region to actual executable plaintext. Only then can a device
   dump test answer the firmware question.

Linux currently has a separate prerequisite: Titan's Thunderbolt PCI bus
assignment failure leaves the Apollo offline. Mac engine/UI permissions
remain unchanged by this source investigation. No hardware capability or
decryption success is claimed.
