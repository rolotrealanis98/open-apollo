"""Survey of DSPAudioProcessor-family / newer-JUCE UAD DLLs: IUADPlug write sites (set_int/set_float/arrays at
vtable +0xa0..+0xb8 with an immediate DSP ref), grouped per containing function, plus the host-contract anchors
(UAD2SDK imports, CUADPlugCallbackHandler/processor RTTI names). Static only. Usage: survey_dspaudioprocessor.py [NAME ...]"""
from pathlib import Path
import bisect, json, re, struct, sys
import pefile
from capstone import Cs, CS_ARCH_X86, CS_MODE_64
from capstone.x86 import X86_OP_IMM, X86_OP_MEM

HERE = Path(__file__).resolve().parent
DLLS = Path(__import__('os').environ.get('OA_PLUGIN_DLLS', 'UAD-2 Powered Plugins'))  # the user's own install
SLOTS = {0xa0: 'set_int', 0xa8: 'set_float', 0xb0: 'set_int_array', 0xb8: 'set_float_array'}
CALL = re.compile(rb'\xff[\x90-\x93\x95-\x97]([\xa0\xa8\xb0\xb8])\x00\x00\x00|\xff\x94[\x24\x64\xa4]([\xa0\xa8\xb0\xb8])\x00\x00\x00|\x41\xff[\x90-\x97]([\xa0\xa8\xb0\xb8])\x00\x00\x00', re.S)
FAMILY = ['AMS DMX 15-80 S', 'AMS Neve DFC Channel Strip', 'Auto-Tune Realtime Access', 'Auto-Tune Realtime Advanced', 'Auto-Tune Realtime X',
          'C-Suite C-Axe', 'C-Suite C-Max', 'C-Suite C-Vox', 'MDWEQ5-3B', 'MDWEQ5-5B', 'Ocean Way Studios', 'Sound Machine Wood Works',
          'Oxford Dynamic EQ', 'Oxford Inflator', 'Oxford Limiter', 'Oxford SuprEsser DS', 'Sphere Mic Collection', 'Sphere Mic Collection 180']


def survey(name):
    raw = (DLLS / ('UAD ' + name + '.dll')).read_bytes()
    pe = pefile.PE(data=raw, fast_load=True)
    pe.parse_data_directories(directories=[1, 3])
    im = pe.get_memory_mapped_image()
    base = pe.OPTIONAL_HEADER.ImageBase
    funcs = sorted((e.struct.BeginAddress, e.struct.EndAddress) for e in pe.DIRECTORY_ENTRY_EXCEPTION)
    starts = [a for a, _ in funcs]
    sdk = sorted(i.name.decode() for d in pe.DIRECTORY_ENTRY_IMPORT if d.dll.lower() == b'uad2sdk.dll' for i in d.imports if i.name)
    names = sorted({m.decode() for m in re.findall(rb'\.\?AV([A-Za-z0-9_]+)@@', raw) if re.search(rb'Proc|Handler|Calc|Plug|Param|Effect|Core|Engine|Wrapper', m)})
    hits = {}
    for m in CALL.finditer(im):
        j = bisect.bisect_right(starts, m.start()) - 1
        if j >= 0 and funcs[j][0] <= m.start() < funcs[j][1]:
            hits.setdefault(funcs[j][0], 0)
            hits[funcs[j][0]] += 1
    cs = Cs(CS_ARCH_X86, CS_MODE_64)
    cs.detail = True
    cands = []
    for a, n in sorted(hits.items(), key=lambda kv: -kv[1])[:12]:
        b = next(e for s, e in funcs if s == a)
        refs, kinds, ref = set(), set(), None
        for i in cs.disasm(im[a:b], base + a):
            if i.mnemonic == 'mov' and i.operands[0].type == 1 and i.reg_name(i.operands[0].reg) in ('edx', 'rdx') and i.operands[1].type == X86_OP_IMM:
                ref = i.operands[1].imm
            if i.mnemonic == 'call' and i.operands[0].type == X86_OP_MEM and i.operands[0].mem.disp in SLOTS:
                kinds.add(SLOTS[i.operands[0].mem.disp])
                if ref is not None and 0 <= ref < 0x10000000:
                    refs.add(hex(ref))
        cands.append(dict(va=hex(base + a), size=b - a, write_sites=n, kinds=sorted(kinds), immediate_refs=len(refs), refs=sorted(refs)[:6]))
    return dict(name=name, size=len(raw), uad2sdk_imports=[x.split('@@')[0].lstrip('?') for x in sdk], rtti=names[:24],
                write_functions=len(hits), write_sites=sum(hits.values()), top_functions=cands[:5])


if __name__ == '__main__':
    out = {}
    for n in (sys.argv[1:] or FAMILY):
        out[n] = survey(n)
        print(json.dumps(out[n]), flush=True)
