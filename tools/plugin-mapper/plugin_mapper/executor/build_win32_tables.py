"""Reproduce bounded Win32 locale tables from pinned Wine 11.19 NLS data.
Algorithms: kernelbase/locale.c load_sortdefault_nls/get_char_type/casemap;
ntdll/locale_private.h init_codepage_table. No Wine processes are launched.
"""
import hashlib,json,struct
from pathlib import Path

def build(directory=Path('/usr/share/wine/nls')):
    raw=(directory/'sortdefault.nls').read_bytes()
    ct=struct.unpack_from('<I',raw,8)[0]
    ix=ct+struct.unpack_from('<H',raw,ct+2)[0]+2
    values=[]
    for ch in range(65536):
        a=ix+struct.unpack_from('<H',raw,ix+2*(ch>>8))[0]
        b=ix+struct.unpack_from('<H',raw,a+2*((ch>>4)&15))[0]+(ch&15)
        values.append(struct.unpack_from('<HHH',raw,ct+4+raw[b]*6))
    base=struct.unpack_from('<I',raw,4)[0]
    lower=struct.unpack_from('<H',raw,base+2)[0]
    def lut(off):
        table=struct.unpack('<'+str((len(raw)-off)//2)+'H',raw[off:len(raw)-(len(raw)-off)%2])
        return [(ch+table[table[table[ch>>8]+((ch>>4)&15)]+(ch&15)])&65535 for ch in range(65536)]
    source=Path(__file__).with_name('wine-locale-11.19.c')
    result={'source':'Wine11.19 sortdefault.nls original lookup via kernelbase/locale.c get_char_type',
      'nls_sha256':hashlib.sha256(raw).hexdigest(),
      'source_sha256':hashlib.sha256(source.read_bytes()).hexdigest(),
      'values':values,'upper':lut(base+4),'lower':lut(base+4+lower*2)}
    cp=(directory/'c_1252.nls').read_bytes()
    table=struct.unpack('<'+str(len(cp)//2)+'H',cp);off=table[0]
    if table[1:3]!=(1252,1):raise ValueError('CP1252 SBCS required')
    start=2*(off+table[off]+1)
    result['cp1252']={'nls_sha256':hashlib.sha256(cp).hexdigest(),'default':table[3],
       'mb':list(table[off+1:off+257]),'wc':list(cp[start:start+65536])}
    assert len(result['cp1252']['wc'])==65536
    return result

if __name__=='__main__':
    output=Path(__file__).with_name('win32-ctype.json')
    data=json.dumps(build(),separators=(',',':')).encode()
    if output.exists()and output.read_bytes()!=data:raise SystemExit('existing pinned NLS artifact differs')
    output.write_bytes(data)
    print(hashlib.sha256(data).hexdigest())

