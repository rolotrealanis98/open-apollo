"""Discover the shared original CRT encoded-null initializer structurally.
No plugin/address list, no pointer or coefficient value is synthesized.
"""
import re,struct,hashlib
import pefile
import pe_cache
PATTERN=re.compile(rb'\x40\x53\x48\x83\xec\x20\x48\x8b\x1d....(?:\x48\x8b\xcb\xe8....){5}\xb0\x01\x48\x83\xc4\x20\x5b\xc3',re.DOTALL)

def discover(path):
    pe=pe_cache.parsed(path,fast_load=False);base=pe.OPTIONAL_HEADER.ImageBase
    cookie=pe.DIRECTORY_ENTRY_LOAD_CONFIG.struct.SecurityCookie
    matches=[]
    for sec in pe.sections:
        if not sec.Characteristics&0x20000000:continue
        data=sec.get_data()
        for match in PATTERN.finditer(data):
            va=base+sec.VirtualAddress+match.start();raw=match.group()
            source=va+13+struct.unpack_from('<i',raw,9)[0]
            if source!=cookie:continue
            writes=[];calls=[]
            for off in range(13,53,8):
                target=va+off+8+struct.unpack_from('<i',raw,off+4)[0]
                body=pe.get_data(target-base,64);pos=0;targets=[]
                while body[pos:pos+3]==b'\x48\x89\x0d':
                    dst=target+pos+7+struct.unpack_from('<i',body,pos+3)[0]
                    owner=next((s for s in pe.sections if base+s.VirtualAddress<=dst<base+s.VirtualAddress+max(s.Misc_VirtualSize,s.SizeOfRawData)),None)
                    if owner is None or not owner.Characteristics&0x80000000 or owner.Characteristics&0x20000000:
                        raise ValueError('CRT initializer destination is not writable data')
                    targets.append(dst);pos+=7
                if not targets or body[pos:pos+1]!=b'\xc3':raise ValueError('CRT initializer callee has non-store behavior')
                writes.extend(targets);calls.append(dict(va=hex(target),body_sha256=hashlib.sha256(body[:pos+1]).hexdigest(),writes=[hex(x)for x in targets]))
            matches.append(dict(va=hex(va),cookie=hex(cookie),source='PE LoadConfig SecurityCookie; exact original five-call encoded-null initializer',
                                initializer_sha256=hashlib.sha256(raw).hexdigest(),calls=calls,writes=[hex(x)for x in writes]))
    if len(matches)!=1:raise ValueError('unique original encoded-null initializer required, found '+str(len(matches)))
    return matches[0]

def initialize(cap,path):
    evidence=discover(path);cap.call(int(evidence['va'],16))
    cookie=cap.rq(int(evidence['cookie'],16))
    if any(cap.rq(int(x,16))!=cookie for x in evidence['writes']):raise ValueError('original CRT encoded-null initialization differs')
    return evidence
