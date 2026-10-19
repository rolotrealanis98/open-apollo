"""Static profile discovery for the UAD DSPAudioProcessor family (JUCE VST2 wrapper + CUADPlugCallbackHandler).

Sibling of overrides/generate_profiles.py (which only understands the CHypParamBlock framework): same output
envelope ({'schema': 'openapollo-batch-discovery/1', 'profiles': [...]}), consumable by
`catalog-execution/run_catalog.py --profiles <file>`. Never loads or executes vendor code.

What is derived from the image (and re-verified on every run):
  * identity: DLL and capture hashes against families/inventory.json; render id against the descriptor store;
  * the CUADPlugCallbackHandler vtable (RTTI sub-object of the processor class) and its slots;
  * the DSP update function: the function that writes IUADPlug set_int/set_float refs, its ref list, and the
    processor pointer fields it dereferences (the JUCE value-tree atomics);
  * per-parameter raw ranges/defaults read from the image data the original constructor passed to the parameter
    factory, and the parameter id strings (checked against the captured Console names).
What is reverse-engineered (Ghidra, see families-dspaudioprocessor.md) and pinned by identity checks here:
  the constructor-assigned field -> parameter binding, the integer domain, startup call, formatter slot.

Two flavors share the host contract. `listener` (C-Vox): a value-tree listener function reads parameter atomics.
`setter` (DMX 15-80 S, Wood Works): the processor's own JUCE setParameter virtual is the parameter -> DSP function.
For the setter flavor everything that can be read from the image is derived rather than pinned: the handler offset
and vtable (inventory RTTI), the processor's primary vtable (RTTI complete-object locator at offset 0), the interface
and wrapper pointer offsets (handler + 0x10 / + 0x18), the parameter count (the vtable's `mov eax, N; ret` virtual),
parameter names (the pointer table the name virtual indexes), integer domains and defaults (the plug-in's own domain
tables / program table), the constructor itself (found by its handler-vtable store; the host runs it), the statically
linked CRT heap/sprintf entry points. The remaining pins (setter slot, constructor stop address, state-block field
offsets, table addresses) are each verified against the capture or by execution (strict uninitialised-read check).

Usage: python3 dspaudioprocessor_profiles.py [--plugin NAME ...] [--output profiles.json]
"""
from pathlib import Path
import argparse, bisect, hashlib, json, re, struct, sys
import pefile
from capstone import Cs, CS_ARCH_X86, CS_MODE_64
from capstone.x86 import X86_OP_IMM, X86_OP_MEM, X86_OP_REG

import os
HERE = Path(__file__).resolve().parent
# Set by plugin_mapper: the work directory (families/inventory.json), the user's own DLLs, and the
# parameter metadata it writes from the installed panel specs (one <slug>.json per plug-in).
PATTERNS = Path(os.environ.get('OA_MAPPER_WORK', str(Path.home() / '.cache/open-apollo/plugin-mapper')))
DLL_ROOT = Path(os.environ.get('OA_PLUGIN_DLLS', 'UAD-2 Powered Plugins'))
CAPTURES = Path(os.environ.get('OA_METADATA', str(PATTERNS / 'metadata')))
SCHEMA = 'openapollo-dspaudioprocessor-profile/1'


def console_text(prop):
    """Console display text of a captured parameter, or None when the metadata has no text (panel-spec metadata)."""
    value = prop.get('StringValue')
    return None if value is None else value['value'].strip()


def text_or_none(text):
    """The executor checks the original formatter against this text only when every parameter has one."""
    return None if any(item is None for item in text) else text
SETTER_FLAVOR = 'setter'
IFACE_WRITE_SLOTS = {0xa0: 'set_int', 0xa8: 'set_float', 0xb0: 'set_int_array', 0xb8: 'set_float_array'}

# Reverse-engineered, identity-pinned facts. Keyed by plug-in name; every address is verified below.
KNOWN = {
    'UAD C-Suite C-Vox': dict(
        sha256='65fee057180a26535bc380b3b2e49b93286c72ebb5548fdd502d6474725f480a',
        processor_vtable=0x1802700d0,             # Uad2againJuceAudioProcessor primary vftable (checked against the RTTI locator below)
        handler_offset=0x150,                     # CUADPlugCallbackHandler sub-object (RTTI locator 0x180292b68)
        handler_vtable=0x1802703d8,
        iface_offset=0x160,                       # IUADPlug* stored by the VST wrapper constructor 0x1800b7010
        wrapper_offset=0x168,                     # JuceVSTWrapper* stored next to it; handler+0x18 = AudioEffect services
        sample_rate_offset=0x30,                  # juce::AudioProcessor::currentSampleRate (double)
        update_va=0x1800bb1d0,                    # DSP update from parameters (the value-tree listener body)
        update_writes=[0x1000000, 0x1000001, 0x1000006, 0x1000007, 0x1000008],
        formatter_slot=0x78,                      # handler slot 15: (this, index, float normalized, char* out)
        startup=[dict(slot=0x88, arguments=['handler'])],  # handler slot 17: rate -> ref 6, zero meter memory, update
        constructor=0x1800bc840,
        # name, id string VA, processor field holding the atomic<float>* (set by the constructor),
        # (min, max, default) float VAs the constructor passed to the parameter factory (None = bool/choice 0..1), maximum
        parameters=[
            dict(name='Mode', id_va=0x18022fcb4, pointer_offset=0x1c0, range=(None, None, None), raw=(0.0, 1.0, 0.0), maximum=1),
            dict(name='Atten', id_va=0x18022ba9c, pointer_offset=0x1c8, range=(0x18022b994, 0x18022ba1c, 0x18022ba44), maximum=100),
            dict(name='Ambience', id_va=0x180231890, pointer_offset=0x1d0, range=(0x18022b94c, 0x18022b9cc, 0x18022b9bc), maximum=100),
            dict(name='Bypass', id_va=0x18022b944, pointer_offset=0x1d8, range=(None, None, None), raw=(0.0, 1.0, 0.0), maximum=1),
        ],
        plain_fields={},
        quiet_iface_slots=[0x48],                 # IUADPlug call in the descriptor getter whose result is discarded
    ),
    # --- setter flavor: handler, processor vtable, interface offsets, parameter count/names/domains/defaults derived ----
    'UAD AMS DMX 15-80 S': dict(
        flavor=SETTER_FLAVOR,
        sha256='369b84c394b21428db1e4ca0595242c39b6d2ca0e2c45c1ec6239cce41b34830',
        sample_rate_offset=0x30,                  # juce::AudioProcessor::currentSampleRate (double)
        channel_fields=[0x3c, 0x40],              # play-config numInputChannels / numOutputChannels (read as the channel count)
        num_parameters_slot=15,                   # processor vtable slot `mov eax, 27; ret` (checked against the capture)
        setter_slot=19,                           # num_parameters_slot + 4: `jmp` thunk to setParameter(this, int, float) 0x18001d680
        formatter_slot=0x78,                      # handler slot 15: (this, index, float normalized, char* out) -> FUN_180001bc0
        startup=[dict(slot=0x88, arguments=['handler'])],  # handler slot 17 (-> FUN_18001e630): rate/refs, arrays, FUN_18001eae0 push
        # Parameter store: a heap object the base constructor allocates and initialises (31 int32 values from +8: 27 Console
        # parameters + 4 editor-only); the host only reads it back. Domain tables: min/max int32[31], default float[31].
        state_block=dict(pointer_offset=0xd8, values_offset=8, count=31,
                         tables=dict(min=0x1803baa90, max=0x1803bac30, default=0x1803b7960)),
        constructor_stop=0x18001d208,             # before the call that loads user presets from disk (FUN_180018eb0)
        plain_fields={},
        quiet_iface_slots=[],
    ),
    'UAD Sound Machine Wood Works': dict(
        flavor=SETTER_FLAVOR,
        sha256='89c2edd69a9aa3400ef95cebbb0ff85b0cff2c7a46653760491380755b3a16eb',
        sample_rate_offset=0x30,
        channel_fields=[0x3c, 0x40],              # play-config numInputChannels / numOutputChannels (read as the channel count)
        stereo=True,                              # captured pan texts are the stereo instance's: (Mono) locks both pans at centre
        num_parameters_slot=14,                   # processor vtable slot `mov eax, 9; ret`
        setter_slot=18,                           # num_parameters_slot + 4: setParameter(this, int, float) 0x1800052b0
        formatter_slot=0x78,                      # handler slot 15: (this, index, float normalized, char* out) -> FUN_180005f50
        startup=[dict(slot=0x88, arguments=['handler'])],  # handler slot 17 (FUN_180006200 -> FUN_1800062f0)
        # Parameters are plain float fields of the processor (getParameter virtual indexes them). The factory default program is
        # row 0 of a float[9] program table; the constructor's own load of it goes through the preset bank, which the host skips.
        program_table=dict(va=0x181ee0390),
        resolution=1000,                          # host integer steps of the continuous parameters (stepped ones come from the capture)
        constructor_stop=0x180004f03,             # before the call that loads user presets from disk (FUN_180001cf0)
        plain_fields={},
        # IUADPlug +0x118(1) is answered 0, as in the VST2 IUADPlug family (flush, nothing pending); the power helper
        # FUN_180006bb0 writes the power word 0x1c58 only for that answer.
        quiet_iface_slots=[0x118],
    ),
    'UAD Auto-Tune Realtime Access': dict(
        flavor=SETTER_FLAVOR,
        sha256='09e579f332ca99408e5ca3e5cbe6e6e3dde07cd10c8a2e946a226a264fa7c493',
        sample_rate_offset=0x38,                  # double read by the reset routine 0x18000e3c0 (cvttsd2si [this+0x38])
        channel_fields=[0x44, 0x48],                # numInputChannels / numOutputChannels compared with 2 by the reset routine
        num_parameters_slot=16,                   # processor vtable slot `mov eax, 17; ret`; +1 name, +2 getParameter, +3 text
        setter_slot=21,                           # setParameter(this, int, float) 0x18000e964: stores [this+0x84c+4i], switch(i), IUADPlug writes
        formatter_slot=0x78,                      # handler slot 15 (0x18000f358)
        startup=[dict(slot=0x88, arguments=['handler'])],  # handler slot 17 (0x18000e6a4)
        constructor_stop=0x18000ddea,             # after the constructor's own setParameter loop, before the preset-file loads 0x180008928 / 0x180008ae4
        # The AudioProcessor/String code reads C++ statics (juce empty String ...) that the CRT start-up would initialise: the
        # .CRT$XCU initializer table, a zero-bounded run of 250 function pointers (found as the table holding the String initializer 0x1800fa924).
        static_initializers=True,                  # derived: the zero-bounded 250-pointer XCU table at 0x1800fd738 (atexit 0x1800d6680)
        resolution=1,
        # Editor-only slot 17 of the 19-element float array (+0x84c): never written by the constructor, read by its setParameter loop, and
        # setParameter ignores index 17 (only 0..4 and 5..0x10 and 0x12 are handled), so any value is equivalent.
        plain_fields={'editor_slot_17': dict(offset=0x890, size=4, value=0)},
        uninitialised_ok=[dict(pc=0x18000e4c4, reason='`and dword ptr [rdx+0x10], 0` zeroing idiom: the result does not depend on the old value'),
                          dict(pc=0x18000e4c8, reason='`and dword ptr [rdx+0x18], 0` zeroing idiom: the result does not depend on the old value'),
                          dict(pc=0x1800154c8, reason='engine set-up reads a float from the first bytes of the 16-byte settings holder the constructor allocates (never written; plain malloc without HEAP_ZERO_MEMORY in the original too)')],
        quiet_iface_slots=[],
    ),
    'UAD Auto-Tune Realtime Advanced': dict(
        flavor=SETTER_FLAVOR,
        sha256='67fe644fa729988ccbb809b9ce24c07cda6c3b480805052ac0ad83089db69a2d',
        sample_rate_offset=0x38,                  # cvttsd2si [this+0x38] in the reset routine 0x180013018
        channel_fields=[0x44, 0x48],              # compared with 2 by the reset routine
        num_parameters_slot=16,                   # processor vtable slot `mov eax, 0x14; ret`; +1 name, +2 getParameter (float array +0x178c), +3 text
        setter_slot=21,                           # setParameter(this, int, float) 0x1800134a8
        formatter_slot=0x78,                      # handler slot 15
        startup=[dict(slot=0x88, arguments=['handler'])],  # handler slot 17 (0x180013208)
        constructor_stop=0x180012a3c,             # after the 36-element setParameter loop and the table copy, before the preset-file loads (0x180003244/0x180003400)
        static_initializers=True,
        # The capture carries step lists only for the four Off/On switches. The other domains are the plateaus of the original
        # setParameter + text virtual (probed under this host over a 2000-point sweep of each parameter, scratch/probe, 2026-10-18):
        # Input Type 4, Key 12, Scale 29, Detune -100..100, Flex-Tune/Humanize 0..100, Retune Speed 0..400, Natural Vibrato -12.0..12.0 by 0.1,
        # Vibrato Shape 4, Vibrato Rate 0.1..10.0 by 0.1, Variation/Pitch/Formant 0..100, Amplitude 0..30, V.Onset Delay/Rate 0..1500.
        # Every captured default is exactly index/maximum with these maxima.
        # The Console capture shows an empty text for Detune, Flex-Tune and Humanize at their defaults; the original text virtual returns
        # '0' for exactly these (every other captured text matches byte for byte). Accepted for those three only; the live gate confirms.
        blank_for_zero=[3, 4, 6],
        maxima={0: 3, 1: 11, 2: 28, 3: 200, 4: 100, 5: 400, 6: 100, 7: 240, 11: 3, 12: 99, 13: 100, 14: 1500, 15: 1500, 16: 100, 17: 30, 18: 100},
        resolution=1000,
        uninitialised_ok=[dict(pc=0x1800ead38, reason='static strncpy scans whole aligned qwords for the terminator; bytes after the NUL are tested but never stored'),
                          dict(pc=0x180026d38, reason='engine set-up reads a float from the first bytes of the 16-byte settings holder the constructor allocates (same code as Access 0x1800154c8); offline result checked insensitive to the heap contents')],
        plain_fields={},
        quiet_iface_slots=[],
    ),
}


def _td_rva(img, locator_va):
    sig, off, cd, td, ch, self_rva = struct.unpack_from('<6I', img.im, locator_va - img.base)
    if sig != 1 or self_rva != locator_va - img.base:
        raise ValueError('not an RTTI complete-object locator ' + hex(locator_va))
    return td


def primary_vtable(img, handler_locator_va):
    """Primary vtable (RTTI complete-object locator with offset 0, same type descriptor) of the class whose secondary
    sub-object locator the inventory recorded. The vtable starts right after the pointer to the locator."""
    td = _td_rva(img, handler_locator_va)
    found = []
    for m in re.finditer(re.escape(struct.pack('<I', td)), img.im):
        pos = m.start() - 12
        if pos < 0 or pos % 4:
            continue
        sig, off, cd, t, ch, self_rva = struct.unpack_from('<6I', img.im, pos)
        if sig == 1 and off == 0 and t == td and self_rva == pos:
            found.append(img.base + pos)
    if len(found) != 1:
        raise ValueError('expected one offset-0 locator for the processor class, found ' + repr([hex(f) for f in found]))
    needle = struct.pack('<Q', found[0])
    refs = [m.start() for m in re.finditer(re.escape(needle), img.im) if m.start() % 8 == 0]
    if len(refs) != 1:
        raise ValueError('expected one vtable referencing the locator')
    return img.base + refs[0] + 8


def find_constructor(img, handler_vtable, handler_offset, destructor):
    """The function that stores the handler vtable at [this + handler_offset]: the processor constructor (the deleting
    destructor, vtable slot 0, restores the same vtable and is excluded)."""
    out = set()
    for m in re.finditer(b'\\x48\\x8d[\\x05\\x0d\\x15\\x1d\\x25\\x2d\\x35\\x3d]', img.im):
        o = m.start()
        if img.base + o + 7 + struct.unpack_from('<i', img.im, o + 3)[0] != handler_vtable:
            continue
        nxt = list(img.cs.disasm(img.im[o + 7:o + 7 + 12], img.base + o + 7))[:1]
        if nxt and nxt[0].mnemonic == 'mov' and nxt[0].operands[0].type == X86_OP_MEM and nxt[0].operands[0].mem.disp == handler_offset:
            out.add(img.function(img.base + o)[0])
    out.discard(destructor)
    if len(out) != 1:
        raise ValueError('expected one constructor storing the handler vtable, found ' + repr([hex(x) for x in out]))
    return out.pop()


def iat_callers(img, dll, symbol):
    """Entry addresses of the functions containing `call [rip+disp]` through the import `dll!symbol`."""
    slot = None
    for entry in img.pe.DIRECTORY_ENTRY_IMPORT:
        for imp in entry.imports:
            if entry.dll.lower() == dll and imp.name == symbol:
                slot = imp.address
    if slot is None:
        return set()
    out = set()
    for m in re.finditer(b'\\xff\\x15', img.im):
        o = m.start()
        if img.base + o + 6 + struct.unpack_from('<i', img.im, o + 2)[0] == slot:
            try:
                out.add(img.function(img.base + o)[0])
            except ValueError:
                pass
    return out


def find_crt_heap(img):
    """Statically linked VS2010 CRT heap entry points (no msvcrt import): malloc is the HeapAlloc caller that rejects sizes
    above -0x20 in its first instructions (calloc multiplies first); free is the short HeapFree caller that starts with `test rcx, rcx`."""
    def ins(va):
        a, b, code = img.function(va)
        return b - a, code
    mallocs = []
    for va in iat_callers(img, b'kernel32.dll', b'HeapAlloc'):
        size, code = ins(va)
        if any(i.mnemonic == 'cmp' and i.op_str == 'rcx, -0x20' for i in code[:8]):
            mallocs.append(va)
    frees = []
    for va in iat_callers(img, b'kernel32.dll', b'HeapFree'):
        size, code = ins(va)
        if size <= 80 and code[0].mnemonic == 'test' and code[0].op_str == 'rcx, rcx':
            frees.append(va)
    if len(mallocs) != 1 or len(frees) != 1:
        raise ValueError('static CRT heap functions not identified: %r %r' % ([hex(x) for x in mallocs], [hex(x) for x in frees]))
    return dict(malloc=hex(mallocs[0]), free=hex(frees[0]))


def float_field_base(img, get_parameter):
    """Processor offset of the float array the getParameter virtual indexes: `movss xmm0, [reg + idx*4 + disp]`."""
    try:
        _, _, code = img.function(get_parameter)
    except ValueError:        # leaf function without unwind data (Auto-Tune): decode raw up to the first ret
        o = get_parameter - img.base
        code = []
        for i in img.cs.disasm(img.im[o:o + 64], get_parameter):
            code.append(i)
            if i.mnemonic == 'ret':
                break
    bases = {i.operands[1].mem.disp for i in code if i.mnemonic in ('mov', 'movss') and len(i.operands) == 2 and i.operands[1].type == X86_OP_MEM
             and i.operands[1].mem.index and i.operands[1].mem.scale == 4 and i.operands[1].mem.disp >= 0x100}
    if len(bases) != 1:
        raise ValueError('getParameter virtual does not index one float array: ' + repr(bases))
    return bases.pop()


def find_printf(img, formatter_va):
    """Target of the direct call the formatter makes with a literal format string (`lea r8, [rip+fmt]` containing `%`):
    the statically linked sprintf_s. None when the formatter builds its text by hand."""
    _, _, ins = img.function(formatter_va)
    fmt, targets = None, set()
    for i in ins:
        if i.mnemonic == 'lea' and i.operands[0].type == X86_OP_REG and i.reg_name(i.operands[0].reg) == 'r8' \
                and i.operands[1].type == X86_OP_MEM and i.reg_name(i.operands[1].mem.base) == 'rip':
            try:
                fmt = img.cstr(i.address + i.size + i.operands[1].mem.disp)
            except (ValueError, UnicodeDecodeError):
                fmt = None
        elif i.mnemonic == 'call':
            if i.operands[0].type == X86_OP_IMM and fmt and '%' in fmt:
                targets.add(i.operands[0].imm)
            fmt = None
    if len(targets) > 1:
        raise ValueError('formatter calls several printf-like targets: ' + repr([hex(t) for t in targets]))
    return targets.pop() if targets else None


def derive_static_initializers(img):
    """Locate the .CRT$XCU table without a pin: among zero-bounded runs of >= 20 code pointers in read-only data, the one whose entries most
    often tail-jump (`lea rcx, [destructor stub]; jmp atexit`) to one common target. Returns the {table, count, atexit} spec."""
    text = next(sec for sec in img.pe.sections if sec.Name.startswith(b'.text'))
    lo, hi = img.base + text.VirtualAddress, img.base + text.VirtualAddress + text.Misc_VirtualSize
    best = None
    for sec in img.pe.sections:
        if sec.Characteristics & 0x80000000 or sec.Characteristics & 0x20000000:   # read-only data only
            continue
        start, end = img.base + sec.VirtualAddress, img.base + sec.VirtualAddress + min(sec.Misc_VirtualSize, len(img.im) - sec.VirtualAddress)
        va = start
        while va + 8 <= end:
            if img.u64(va) == 0 or not lo <= img.u64(va) < hi:
                va += 8
                continue
            run = va
            while run + 8 <= end and lo <= img.u64(run) < hi:
                run += 8
            count = (run - va) // 8
            if count >= 20 and va > start and img.u64(va - 8) == 0 and run + 8 <= end and img.u64(run) == 0:
                tails = {}
                for i in range(count):
                    o = img.u64(va + 8 * i) - img.base
                    for _ in range(8):
                        if img.im[o:o + 3] == b'\x48\x8d\x0d' and img.im[o + 7] == 0xe9:
                            t = o + 12 + struct.unpack_from('<i', img.im, o + 8)[0] + img.base
                            tails[t] = tails.get(t, 0) + 1
                            break
                        o += 1
                if tails:
                    target = max(tails, key=tails.get)
                    if tails[target] >= count // 3 and (best is None or tails[target] > best[0]):
                        best = (tails[target], va, count, target)
            va = run
    if best is None:
        raise ValueError('no .CRT$XCU initializer table found')
    return dict(table=best[1], count=best[2], atexit=best[3])


def static_initializers(img, spec):
    """The zero-bounded table of C++ dynamic initializers: every entry must be code inside .text and the table must be
    delimited by null pointers on both sides. Returns {table, count} for the host to run in order before the constructor."""
    if spec is True:
        spec = derive_static_initializers(img)
    table, count = spec['table'], spec['count']
    text = next(sec for sec in img.pe.sections if sec.Name.startswith(b'.text'))
    lo, hi = img.base + text.VirtualAddress, img.base + text.VirtualAddress + text.Misc_VirtualSize
    entries = [img.u64(table + 8 * i) for i in range(count)]
    if img.u64(table - 8) != 0 or img.u64(table + 8 * count) != 0 or any(not lo <= e < hi for e in entries):
        raise ValueError('static initializer table is not a zero-bounded run of code pointers')
    # atexit: the target the initializers tail-jump to after `lea rcx, [destructor stub]` (a jmp rel32 right behind a lea rcx)
    tails = {}
    for e in entries:
        o = e - img.base
        for _ in range(8):
            if img.im[o:o + 3] == b'\x48\x8d\x0d' and img.im[o + 7] == 0xe9:
                t = o + 12 + struct.unpack_from('<i', img.im, o + 8)[0] + img.base
                tails[t] = tails.get(t, 0) + 1
                break
            o += 1
    if tails.get(spec['atexit'], 0) < count // 3 or max(tails, key=tails.get) != spec['atexit']:
        raise ValueError('atexit is not the initializers common tail call target')
    return dict(table=hex(table), count=count, atexit=hex(spec['atexit']), first=hex(entries[0]), last=hex(entries[-1]))


def follow_thunk(img, va):
    """Address of the real function behind a single `jmp rel32` thunk (the function itself otherwise)."""
    o = va - img.base
    if img.im[o] == 0xe9:
        return va + 5 + struct.unpack_from('<i', img.im, o + 1)[0]
    return va


def constant_returning_virtuals(img, vtable, limit=64):
    """{slot: N} for vtable entries that are exactly `mov eax, imm32; ret`."""
    out = {}
    for i, fn in enumerate(img.vtable(vtable, limit)):
        o = fn - img.base
        if img.im[o] == 0xb8 and img.im[o + 5] == 0xc3:
            out[i] = struct.unpack_from('<I', img.im, o + 1)[0]
    return out


def rip_targets(img, va):
    """Targets of every RIP-relative `lea` inside the function at va (its static data/string references)."""
    _, _, ins = img.function(va)
    return [i.address + i.size + i.operands[1].mem.disp for i in ins
            if i.mnemonic == 'lea' and i.operands[1].type == X86_OP_MEM and i.reg_name(i.operands[1].mem.base) == 'rip']


def find_pointer_table(img, strings, referenced_by=None):
    """VA of the pointer array whose entries are exactly `strings`; when several copies exist, the one the function
    `referenced_by` (the getParameterName virtual) loads."""
    first = strings[0].encode() + b'\0'
    hits = []
    for m in re.finditer(re.escape(first), img.im):
        sva = img.base + m.start()
        for r in re.finditer(re.escape(struct.pack('<Q', sva)), img.im):
            if r.start() % 8:
                continue
            va = img.base + r.start()
            try:
                if [img.cstr(img.u64(va + 8 * i)) for i in range(len(strings))] == list(strings):
                    hits.append(va)
            except (ValueError, struct.error, UnicodeDecodeError):
                pass
    if referenced_by is not None:
        wanted = set(rip_targets(img, referenced_by))
        hits = [h for h in hits if h in wanted]
    if len(set(hits)) != 1:
        raise ValueError('parameter name table not unique in the image: ' + repr([hex(h) for h in hits]))
    return hits[0]


def function_strings(img, va):
    """Printable C strings the function at va references (for virtuals that switch over literal names)."""
    out = set()
    for t in rip_targets(img, va):
        try:
            if img.inside(t):
                out.add(img.cstr(t))
        except (ValueError, UnicodeDecodeError):
            pass
    return out


class Image:
    def __init__(self, path):
        self.raw = Path(path).read_bytes()
        self.pe = pefile.PE(data=self.raw, fast_load=True)
        self.pe.parse_data_directories(directories=[1, 3])
        self.im = self.pe.get_memory_mapped_image()
        self.base = self.pe.OPTIONAL_HEADER.ImageBase
        self.funcs = sorted((e.struct.BeginAddress, e.struct.EndAddress) for e in self.pe.DIRECTORY_ENTRY_EXCEPTION)
        self.starts = [a for a, _ in self.funcs]
        self.cs = Cs(CS_ARCH_X86, CS_MODE_64)
        self.cs.detail = True

    def inside(self, va, size=1):
        return self.base <= va and va + size <= self.base + len(self.im)

    def u64(self, va):
        return struct.unpack_from('<Q', self.im, va - self.base)[0]

    def f32(self, va):
        return struct.unpack_from('<f', self.im, va - self.base)[0]

    def cstr(self, va):
        o = va - self.base
        return self.im[o:self.im.index(b'\0', o)].decode('ascii')

    def function(self, va):
        r = va - self.base
        j = bisect.bisect_right(self.starts, r) - 1
        if j < 0 or not self.funcs[j][0] <= r < self.funcs[j][1]:
            raise ValueError('no PE function boundary ' + hex(va))
        a, b = self.funcs[j]
        return a + self.base, b + self.base, list(self.cs.disasm(self.im[a:b], self.base + a))

    def vtable(self, va, limit=256):
        out = []
        while len(out) < limit:
            v = self.u64(va + 8 * len(out))
            if not self.inside(v) or v < self.base + 0x1000:
                break
            out.append(v)
        return out


def scan_update(img, va):
    """(writes, pointer_fields): IUADPlug writes with their immediate refs, and the [this+disp] pointer fields
    the function dereferences to read value-tree floats (mov rax,[this+d]; mov ecx,[rax])."""
    _, _, ins = img.function(va)
    name = lambda i, o: i.reg_name(i.operands[o].reg)
    writes, fields, ref = [], [], None
    for k, i in enumerate(ins):
        if i.mnemonic == 'mov' and len(i.operands) == 2 and i.operands[0].type == X86_OP_REG and name(i, 0) == 'edx' and i.operands[1].type == X86_OP_IMM:
            ref = i.operands[1].imm
        if i.mnemonic == 'call' and i.operands[0].type == X86_OP_MEM and i.operands[0].mem.disp in IFACE_WRITE_SLOTS:
            writes.append((IFACE_WRITE_SLOTS[i.operands[0].mem.disp], ref))
        if (i.mnemonic == 'mov' and len(i.operands) == 2 and i.operands[0].type == X86_OP_REG and name(i, 0) == 'rax'
                and i.operands[1].type == X86_OP_MEM and i.operands[1].mem.disp >= 0x100 and k + 1 < len(ins)):
            nxt = ins[k + 1]
            if nxt.mnemonic == 'mov' and nxt.operands[1].type == X86_OP_MEM and nxt.reg_name(nxt.operands[1].mem.base) == 'rax' and nxt.operands[1].mem.disp == 0:
                fields.append(i.operands[1].mem.disp)
    return writes, sorted(set(fields))


def result_discarded(img, ranges, slot):
    """True when every `call [reg+slot]` in the function is followed by a write to rax before any read of it
    (the original ignores the interface method's return value, so the offline host may answer 0)."""
    found = False
    a, b = min(r[0] for r in ranges), max(r[1] for r in ranges)  # chained PE fragments: linear span of the discovery pe_ranges
    ins = list(img.cs.disasm(img.im[a - img.base:b - img.base], a))
    for k, i in enumerate(ins):
        if i.mnemonic == 'call' and i.operands[0].type == X86_OP_MEM and i.operands[0].mem.disp == slot:
            found = True
            for j in ins[k + 1:k + 12]:
                read, written = j.regs_access()
                names = lambda regs: {j.reg_name(r) for r in regs}
                if names(read) & {'rax', 'eax', 'ax', 'al'}:
                    return False
                if names(written) & {'rax', 'eax', 'ax', 'al'} or j.mnemonic in ('ret', 'jmp'):
                    break
    return found


def sha(path):
    return hashlib.sha256(Path(path).read_bytes()).hexdigest()


def build(name, inventory):
    known = KNOWN.get(name)
    if known is None:
        raise ValueError('no DSPAudioProcessor-family facts for ' + name)
    if known.get('flavor') == SETTER_FLAVOR:
        return build_setter(name, known, inventory[name])
    item = inventory[name]
    dll = DLL_ROOT / (name + '.dll')
    if sha(dll) != item['dll_sha256'] or item['dll_sha256'] != known['sha256']:
        raise ValueError('original DLL identity differs')
    capture = CAPTURES / (item['capture'] + '.json')
    img = Image(dll)
    if primary_vtable(img, int(item['discovery']['descriptors'][0]['descriptor_subobject_vtables'][0]['rtti']['locator'], 16)) != known['processor_vtable']:
        raise ValueError('pinned processor vtable differs from the RTTI-derived primary vtable')
    # --- handler vtable / descriptor ---------------------------------------------------------------------
    descriptor = item['discovery']['descriptors'][0]
    descriptor_va = int(descriptor['function']['va'], 16)
    sub = descriptor['descriptor_subobject_vtables'][0]
    if (int(sub['va'], 16), sub['slot'], sub['rtti']['subobject_offset']) != (known['handler_vtable'], '0x18', known['handler_offset']):
        raise ValueError('inventory descriptor sub-object differs from the pinned handler')
    slots = img.vtable(known['handler_vtable'])
    if slots[3] != int(descriptor['function']['va'], 16):
        raise ValueError('handler slot 3 is not the discovered descriptor getter')
    primary = img.vtable(known['processor_vtable'])
    if len(primary) < 40 or len(slots) < 19:
        raise ValueError('implausible processor/handler vtables')
    for slot in known['quiet_iface_slots']:
        if not result_discarded(img, [(int(a, 16), int(b, 16)) for a, b in descriptor['function']['pe_ranges']], slot):
            raise ValueError('descriptor getter uses the result of IUADPlug slot ' + hex(slot))
    # --- update function ----------------------------------------------------------------------------------
    writes, fields = scan_update(img, known['update_va'])
    refs = [r for _, r in writes]
    if refs != known['update_writes']:
        raise ValueError('update function DSP writes differ: ' + repr(writes))
    expected_fields = sorted(p['pointer_offset'] for p in known['parameters'])
    if fields != expected_fields:
        raise ValueError('update function pointer fields differ: ' + repr(fields))
    # --- parameters: ranges from image data, ids checked against the captured Console names ------------
    cap = json.loads(capture.read_text())
    props = {}
    for c in cap['controls']:
        p = c['path'].split('/parameters/')
        if len(p) == 2:
            idx, field = p[1].split('/')
            props.setdefault(int(idx), {})[field] = c
    if sorted(props) != list(range(len(known['parameters']))) or item['parameter_count'] != len(known['parameters']):
        raise ValueError('captured parameter schema differs')
    params, text = [], []
    for i, k in enumerate(known['parameters']):
        if img.cstr(k['id_va']).lower() != props[i]['Name']['value'].lower() or k['name'] != props[i]['Name']['value']:
            raise ValueError('parameter id string differs from captured name ' + k['name'])
        if k['range'][0] is None:
            lo, hi, df = k['raw']
        else:
            lo, hi, df = (img.f32(v) for v in k['range'])
        norm = props[i]['NormalizedValue']['value']
        default = round(norm * k['maximum'])
        if not 0 <= default <= k['maximum'] or abs(lo + (hi - lo) * default / k['maximum'] - df) > 1e-3 * max(1.0, abs(df)):
            raise ValueError('captured default differs from the original parameter default for ' + k['name'])
        params.append(dict(name=k['name'], pointer_offset=hex(k['pointer_offset']), maximum=k['maximum'], default=default,
                           raw_min=lo, raw_max=hi, raw_default=df, id_va=hex(k['id_va']),
                           range_sources=[hex(v) if v else None for v in k['range']]))
        text.append(console_text(props[i]))
    return dict(
        schema=SCHEMA, status='discovered', name=name, framework='DSPAudioProcessor',
        dll=dict(path=str(dll), sha256=item['dll_sha256']),
        captured_metadata=dict(path=str(capture), sha256=sha(capture)),
        dspap=dict(
            processor_vtable=hex(known['processor_vtable']),
            handler=dict(offset=hex(known['handler_offset']), vtable=hex(known['handler_vtable']),
                         slots={hex(8 * i): hex(v) for i, v in enumerate(slots[:19])}),
            iface_offset=hex(known['iface_offset']), wrapper_offset=hex(known['wrapper_offset']),
            sample_rate_offset=hex(known['sample_rate_offset']),
            descriptor=dict(getter_va=hex(descriptor_va), render_store_va=descriptor['render_store_va'], render_id=descriptor['render_id'],
                            evidence=descriptor['status']),
            update=dict(va=hex(known['update_va']), writes=[dict(kind=k, ref=hex(r)) for k, r in writes], pointer_fields=[hex(f) for f in fields]),
            formatter=dict(slot=hex(known['formatter_slot']), va=hex(slots[known['formatter_slot'] // 8])),
            startup=[dict(slot=hex(s['slot']), va=hex(slots[s['slot'] // 8]), arguments=s['arguments']) for s in known['startup']],
            parameters=params, plain_fields=known['plain_fields'], quiet_iface_slots=[hex(x) for x in known['quiet_iface_slots']], captured_text=text_or_none(text),
            evidence=dict(constructor=hex(known['constructor']), source='families-dspaudioprocessor.md')),
    )


def captured_parameters(capture):
    cap = json.loads(capture.read_text())
    props = {}
    for c in cap['controls']:
        p = c['path'].split('/parameters/')
        if len(p) == 2:
            idx, field = p[1].split('/')
            props.setdefault(int(idx), {})[field] = c
    return props


def build_setter(name, known, item):
    dll = DLL_ROOT / (name + '.dll')
    if sha(dll) != item['dll_sha256'] or item['dll_sha256'] != known['sha256']:
        raise ValueError('original DLL identity differs')
    capture = CAPTURES / (item['capture'] + '.json')
    img = Image(dll)
    props = captured_parameters(capture)
    count = item['parameter_count']
    if sorted(props) != list(range(count)):
        raise ValueError('captured parameter schema differs')
    # --- handler (inventory RTTI), interface/wrapper offsets, processor primary vtable ----------------------
    descriptor = item['discovery']['descriptors'][0]
    sub = descriptor['descriptor_subobject_vtables'][0]
    handler_offset, handler_vtable = sub['rtti']['subobject_offset'], int(sub['va'], 16)
    if sub['slot'] != '0x18' or not sub['abi_shape'].startswith('receiver+'):
        raise ValueError('descriptor sub-object is not the handler getter slot')
    slots = img.vtable(handler_vtable)
    descriptor_va = int(descriptor['function']['va'], 16)
    if len(slots) < 19 or slots[3] != descriptor_va:
        raise ValueError('handler slot 3 is not the discovered descriptor getter')
    processor_vtable = primary_vtable(img, int(sub['rtti']['locator'], 16))
    primary = img.vtable(processor_vtable)
    ctor = find_constructor(img, handler_vtable, handler_offset, follow_thunk(img, primary[0]))
    ctor_start, ctor_end, _ = img.function(ctor)
    if not ctor_start <= known['constructor_stop'] < ctor_end:
        raise ValueError('constructor stop address is outside the processor constructor')
    # --- parameter count and setter -------------------------------------------------------------------
    constants = constant_returning_virtuals(img, processor_vtable)
    if constants.get(known['num_parameters_slot']) != count:
        raise ValueError('processor numParameters virtual does not return the captured parameter count')
    setter_va = follow_thunk(img, primary[known['setter_slot']])
    # --- parameter names (the table the name virtual indexes) -----------------------------------------
    names = [props[i]['Name']['value'] for i in range(count)]
    name_table = None
    if known.get('state_block'):
        name_table = find_pointer_table(img, names, referenced_by=primary[known['num_parameters_slot'] + 1])
    # --- domains and defaults from the plug-in's own tables -------------------------------------------
    params, state = [], None
    if known.get('state_block'):
        st = known['state_block']
        t = st['tables']
        mins = [struct.unpack_from('<i', img.im, t['min'] - img.base + 4 * i)[0] for i in range(st['count'])]
        maxs = [struct.unpack_from('<i', img.im, t['max'] - img.base + 4 * i)[0] for i in range(st['count'])]
        defs = [img.f32(t['default'] + 4 * i) for i in range(st['count'])]
        if any(not lo < hi for lo, hi in zip(mins, maxs)) or any(not 0.0 <= d <= 1.0 for d in defs):
            raise ValueError('implausible parameter domain tables')
        for i in range(count):
            lo, hi, dn = mins[i], maxs[i], defs[i]
            default = int(lo + (hi - lo) * dn + 0.5) - lo        # the constructor's own default rule
            norm = props[i]['NormalizedValue']['value']
            if abs(norm * (hi - lo) - default) > 1e-3 * (hi - lo):
                raise ValueError('captured default differs from the plug-in default table for ' + names[i])
            steps = props[i].get('StepValue', {}).get('values')
            if steps and len(steps) - 1 != hi - lo:
                raise ValueError('captured step list differs from the integer domain of ' + names[i])
            params.append(dict(name=names[i], maximum=hi - lo, default=default, raw_min=lo, raw_max=hi, raw_default=default + lo,
                               name_va=hex(img.u64(name_table + 8 * i))))
        state = dict(pointer_offset=hex(st['pointer_offset']), values_offset=hex(st['values_offset']), count=st['count'],
                     tables={k: hex(v) for k, v in t.items()})
    else:
        # parameters = float fields; defaults = factory program row 0; names = literals the name virtual switches over
        field_base = float_field_base(img, primary[known['num_parameters_slot'] + 2])
        strings = function_strings(img, primary[known['num_parameters_slot'] + 1])
        if not set(names) <= strings:
            raise ValueError('name virtual does not reference the captured parameter names: ' + repr(sorted(set(names) - strings)))
        if known.get('program_table'):
            program = [img.f32(known['program_table']['va'] + 4 * i) for i in range(count)]
        else:       # no factory table (Auto-Tune): the capture is the only default source
            program = [props[i]['NormalizedValue']['value'] for i in range(count)]
        for i in range(count):
            steps = props[i].get('StepValue', {}).get('values')
            maximum = len(steps) - 1 if steps else known.get('maxima', {}).get(i, known['resolution'])
            default = round(program[i] * maximum)
            norm = props[i]['NormalizedValue']['value']
            if abs(program[i] - norm) > 1e-3 or abs(default / maximum - norm) > 1e-3:
                raise ValueError('captured default differs from the factory program table for ' + names[i])
            params.append(dict(name=names[i], maximum=maximum, default=default, raw_min=0, raw_max=maximum, raw_default=default,
                               field_offset=hex(field_base + 4 * i), program_value=program[i]))
    crt = find_crt_heap(img)
    if known.get('static_initializers'):      # dynamic C++ statics grow Strings/arrays: realloc = the one HeapReAlloc caller
        reallocs = iat_callers(img, b'kernel32.dll', b'HeapReAlloc')
        if len(reallocs) != 1:
            raise ValueError('static CRT realloc not identified: %r' % [hex(x) for x in reallocs])
        crt['realloc'] = hex(reallocs.pop())
        # static strlen: word-at-a-time scan that reads past the terminator (host-implemented, like malloc/free)
        strlens = [img.base + sec.VirtualAddress + m.start() for sec in img.pe.sections if sec.Characteristics & 0x20000000
                   for m in re.finditer(b'\\x48\\x8b\\xc1\\x48\\xf7\\xd9\\x48\\xa9\\x07\\x00\\x00\\x00\\x74', sec.get_data(), re.S)]
        if len(strlens) != 1:
            raise ValueError('static strlen not identified: %r' % [hex(x) for x in strlens])
        crt['strlen'] = hex(strlens[0])
    printf = find_printf(img, slots[known['formatter_slot'] // 8])
    if printf:
        crt['sprintf_s'] = hex(printf)
    text = [console_text(props[i]) for i in range(count)]
    fields = {k: dict(v, offset=hex(v['offset']), value=v['value']) for k, v in known['plain_fields'].items()}
    return dict(
        schema=SCHEMA, status='discovered', name=name, framework='DSPAudioProcessor',
        dll=dict(path=str(dll), sha256=item['dll_sha256']),
        captured_metadata=dict(path=str(capture), sha256=sha(capture)),
        **({'stereo_channel_request': True} if known.get('stereo') else {}),
        dspap=dict(
            processor_vtable=hex(processor_vtable),
            handler=dict(offset=hex(handler_offset), vtable=hex(handler_vtable), slots={hex(8 * i): hex(v) for i, v in enumerate(slots[:19])}),
            iface_offset=hex(handler_offset + 0x10), wrapper_offset=hex(handler_offset + 0x18),
            sample_rate_offset=hex(known['sample_rate_offset']), channel_fields=[hex(o) for o in known['channel_fields']],
            descriptor=dict(getter_va=hex(descriptor_va), render_store_va=descriptor['render_store_va'], render_id=descriptor['render_id'],
                            evidence=descriptor['status']),
            setter=dict(va=hex(setter_va), slot=known['setter_slot'], vtable_entry=hex(primary[known['setter_slot']]),
                        num_parameters_slot=known['num_parameters_slot']),
            constructor=dict(va=hex(ctor), stop_va=hex(known['constructor_stop'])),
            crt=crt,
            **({'static_initializers': static_initializers(img, known['static_initializers'])} if known.get('static_initializers') else {}),
            state_block=state,
            formatter=dict(slot=hex(known['formatter_slot']), va=hex(slots[known['formatter_slot'] // 8])),
            startup=[dict(slot=hex(x['slot']), va=hex(slots[x['slot'] // 8]), arguments=x['arguments']) for x in known['startup']],
            **({'uninitialised_ok': [dict(pc=hex(x['pc']), reason=x['reason']) for x in known['uninitialised_ok']]} if known.get('uninitialised_ok') else {}),
            **({'blank_for_zero': known['blank_for_zero']} if known.get('blank_for_zero') else {}),
            parameters=params, plain_fields=fields, quiet_iface_slots=[hex(x) for x in known['quiet_iface_slots']], captured_text=text_or_none(text),
            evidence=dict(name_table=hex(name_table) if name_table else None,
                          source='families-dspaudioprocessor.md')),
    )


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument('--plugin', action='append')
    ap.add_argument('--output', type=Path, default=HERE / 'profiles-dspaudioprocessor.json')
    args = ap.parse_args()
    inventory = {p['name']: p for p in json.loads((PATTERNS / 'families/inventory.json').read_text())['plugins']}
    names = args.plugin or sorted(KNOWN)
    profiles = [build(n, inventory) for n in names]
    doc = dict(schema='openapollo-batch-discovery/1', profiles=profiles, counts=dict(selected=len(names), discovered=len(profiles)),
               source_sha256=sha(__file__))
    args.output.write_text(json.dumps(doc, indent=2) + '\n')
    print(json.dumps(dict(output=str(args.output), profiles=[p['name'] for p in profiles])))


if __name__ == '__main__':
    main()
