"""Offline executor for a UAD plug-in built against the Visual C++ 2008 dynamic runtime (MSVCR90 / MSVCP90): the Eventide H910
Harmonizer (`H910AudioProcessor`, Eventide's tide::ExtendedAudioProcessor on JUCE, 19 parameters).

Same contract as paramobj_capture.ParamObjectsCapture (itself vstmain_capture.VstMainCapture): nothing is synthesized. The original
DllMain (the MSVCR90 start-up and the C++ dynamic initializers), VSTPluginMain, JuceVSTWrapper and processor constructor build the whole
object graph; `UAD_CreateIUADPlug` hands the plug-in the shared IUADPlug model; a Console control is the VST2 `setParameter`, and the
Console text is the parameter's own text (effGetParamDisplay). The H910's parameters are Eventide parameter objects that pass a value by
name through its DSP model, which is why no hand-written facts describe it.

What this module adds to ParamObjectsCapture: the MSVCR90 start-up and C library calls and the parts of MSVCP90 the H910's own code
reaches, each traced from construction, start-up and controls over every parameter. Every bound name is counted (crt_hits); any other
import still rejects.

  * MSVCR90: the CRT start-up's pointer encoding, `_malloc_crt`, `__dllonexit` (the module's atexit list; the offline process never
    exits) and its lock, operator new / delete / delete[], `memmove_s`, `strcmp`; the rest is the shared C library subset;
  * MSVCP90 std::string (Visual C++ 2008 x64: 40 bytes, +8 the 16-byte buffer or pointer, +0x18 size, +0x20 capacity). The image
    inlines none of it (no capacity check against 16 anywhere), so the host keeps every string in the pointer form and owns it;
  * MSVCP90 string streams (istringstream, ostringstream): the plug-in reaches a stream's std::ios through the virtual-base table
    (`mov rax,[s]; movsxd rax,[rax+4]; add s,rax`), so each stream the host constructs gets a vbptr to a host table whose displacement
    points at ios = object + IOS; the host keeps all stream state keyed by that address and writes nothing else into the object;
  * IUADPlug +0x138 ("is the plug-in invalid", 0 = valid), which the wrapper asks right after UAD_CreateIUADPlug.
"""
import re
import struct
from unicorn.x86_const import UC_X86_REG_XMM0
from dspap_capture import DspApCapture
from paramobj_capture import ParamObjectsCapture

NUL = bytes(1)
NPOS = 0xffffffffffffffff
STRING = '?$basic_string@DU?$char_traits@D@std@@V?$allocator@D@2@@std@@'     # std::string in member names
STRING_ARG = '?$basic_string@DU?$char_traits@D@std@@V?$allocator@D@2@@'      # ... as an argument (std:: back-referenced after it)
FREE = '??$?{op}DU?$char_traits@D@std@@V?$allocator@D@1@@std@@'             # free operator templates over std::string
ISTREAM, OSTREAM = '?$basic_istream@DU?$char_traits@D@std@@@', '?$basic_ostream@DU?$char_traits@D@std@@@'
ISS = '?$basic_istringstream@DU?$char_traits@D@std@@V?$allocator@D@2@@std@@'
OSS = '?$basic_ostringstream@DU?$char_traits@D@std@@V?$allocator@D@2@@std@@'
# the shared C library subset (DspApCapture.import_table) for the names the trace reaches
BASE_CRT = ('malloc', 'realloc', 'memcpy', 'memmove', 'memset', 'strlen', 'exp', 'expf', 'pow', 'powf', 'logf', 'log10', 'log10f',
            'fabs', 'floorf', 'sinf', 'cosf', 'tan')


class Msvc90Capture(ParamObjectsCapture):
    IOS = 0x40
    EOF, FAIL, BAD = 1, 2, 4

    def __init__(self, profile, auto_start=True):
        profile = dict(profile, dspap=dict(profile['dspap'], paramobjects=profile['dspap'].get('paramobjects', {})))
        ParamObjectsCapture.__init__(self, profile, auto_start)

    def bind_sdk(self):
        """ParamObjectsCapture's TEB and UAD2SDK entry, the IUADPlug validity query, and std::string::npos (MSVCP90 data: the import slot
        holds the stub address, so the datum lives there)."""
        ParamObjectsCapture.bind_sdk(self)
        self.bind(0x138, 'plug_is_invalid', lambda: 0)
        for n, label, stub in self.imports:
            if n is not None and n.decode().startswith('?npos@'):
                self.uc.mem_write(stub, struct.pack('<Q', NPOS))

    def crt_table(self):
        table = ParamObjectsCapture.crt_table(self)
        u, a = self.uc, self.arg
        hits = self.crt_hits
        imported = {n.decode() for n, label, stub in self.imports if n is not None}
        self.onexit = []

        def counted(name, fn):
            def run():
                hits[name] = hits.get(name, 0) + 1
                self.crt_first.setdefault(name, self.phase)
                return fn()
            return run

        def onexit():
            self.onexit.append(a(0))
            return a(0)

        def memmove_s():        # (dst, dstsize, src, count) -> errno
            dst, size, src, n = a(0), a(1), a(2), a(3)
            if n > size:
                raise ValueError('memmove_s past its destination')
            if n:
                u.mem_write(dst, bytes(u.mem_read(src, n)))
            return 0

        def strcmp():
            x, y = self.cstr(a(0)), self.cstr(a(1))
            return 0 if x == y else (1 if x > y else 0xffffffff)

        base = DspApCapture.import_table(self)
        stubs = {'_encode_pointer': lambda: a(0), '_decode_pointer': lambda: a(0), '_malloc_crt': base['malloc'],
                 '__dllonexit': onexit, '_lock': lambda: 0, '_unlock': lambda: 0,
                 '??2@YAPEAX_K@Z': base['malloc'], '??3@YAXPEAX@Z': base['free'], '??_V@YAXPEAX@Z': base['free'],
                 'memmove_s': memmove_s, 'strcmp': strcmp}
        stubs.update({name: base[name] for name in BASE_CRT})
        stubs.update(self.msvcp90())
        table.update({name: counted(name, fn) for name, fn in stubs.items() if name in imported and name not in table})
        return table

    def cstr(self, p, n=4096):
        return bytes(self.uc.mem_read(p, n)).split(NUL)[0]

    # --- MSVCP90: std::string -------------------------------------------------------------------------------------
    def s_get(self, obj):
        size, cap = self.rq(obj + 0x18), self.rq(obj + 0x20)
        ptr = self.rq(obj + 8) if cap >= 16 else obj + 8
        if size > 0x100000:
            raise ValueError('std::string of implausible size %#x at %#x' % (size, obj))
        return bytes(self.uc.mem_read(ptr, size)) if size else b''

    def s_put(self, obj, data, fresh=False):
        cap = 0 if fresh else self.rq(obj + 0x20)
        if fresh or cap < 16 or len(data) > cap:
            cap = max(16, len(data))
            ptr = self.reserve(cap + 1)
            self.wq(obj, 0)
            self.wq(obj + 8, ptr)
            self.wq(obj + 0x20, cap)
        self.uc.mem_write(self.rq(obj + 8), data + NUL)
        self.wq(obj + 0x18, len(data))
        return obj

    def s_data(self, obj):
        return self.rq(obj + 8) if self.rq(obj + 0x20) >= 16 else obj + 8

    # --- MSVCP90: string streams ----------------------------------------------------------------------------------
    def vbtable(self, disp):
        tables = self.__dict__.setdefault('_vbtables', {})
        if disp not in tables:
            tables[disp] = self.reserve(8)
            self.uc.mem_write(tables[disp], struct.pack('<ii', 0, disp))
        return tables[disp]

    def make_stream(self, obj, text=b''):
        self.wq(obj, self.vbtable(self.IOS))
        self.__dict__.setdefault('streams', {})[obj + self.IOS] = dict(inp=bytes(text), pos=0, out=bytearray(), state=0)
        return obj

    def stream(self, p, is_ios=False):
        ios = p if is_ios else p + struct.unpack('<i', bytes(self.uc.mem_read(self.rq(p) + 4, 4)))[0]
        st = getattr(self, 'streams', {}).get(ios)
        if st is None:
            raise ValueError('stream operation on an object the host did not construct: %#x' % p)
        return st

    def msvcp90(self):
        a, u = self.arg, self.uc
        f64 = lambda v: (u.reg_write(UC_X86_REG_XMM0, struct.unpack('<Q', struct.pack('<d', v))[0]), 0)[1]
        f32 = lambda v: (u.reg_write(UC_X86_REG_XMM0, struct.unpack('<I', struct.pack('<f', v))[0]), 0)[1]

        def find_first_of_char():
            text, ch, off = self.s_get(a(0)), a(1) & 0xff, a(2)
            i = text.find(bytes([ch]), off) if off <= len(text) else -1
            return NPOS if i < 0 else i

        def rfind_char():
            text, ch, off = self.s_get(a(0)), a(1) & 0xff, a(2)
            i = text.rfind(bytes([ch]), 0, len(text) if off >= len(text) else off + 1)
            return NPOS if i < 0 else i

        def substr():           # (this, result, off, count)
            text, off, n = self.s_get(a(0)), a(2), a(3)
            if off > len(text):
                raise ValueError('std::string::substr out of range')
            return self.s_put(a(1), text[off:] if n == NPOS else text[off:off + n], fresh=True)

        def swap():
            x, y = self.s_get(a(0)), self.s_get(a(1))
            self.s_put(a(0), y)
            self.s_put(a(1), x)
            return 0

        def read_number(pattern, pack):
            def run():          # operator>>(T&): skip white space, then the longest number; failbit when there is none
                st = self.stream(a(0))
                while st['pos'] < len(st['inp']) and st['inp'][st['pos']:st['pos'] + 1].isspace():
                    st['pos'] += 1
                m = re.match(pattern, st['inp'][st['pos']:])
                if not m:
                    st['state'] |= self.FAIL | (self.EOF if st['pos'] >= len(st['inp']) else 0)
                    return a(0)
                st['pos'] += m.end()
                if st['pos'] >= len(st['inp']):
                    st['state'] |= self.EOF
                text = m.group(0)
                u.mem_write(a(1), struct.pack(pack, int(text) if pack == '<i' else float(text)))
                return a(0)
            return run

        def getline():          # (istream&, string&, char delim) -> istream&
            st = self.stream(a(0))
            inp, pos, delim = st['inp'], st['pos'], bytes([a(2) & 0xff])
            if pos >= len(inp):
                st['state'] |= self.EOF | self.FAIL
                self.s_put(a(1), b'')
                return a(0)
            end = inp.find(delim, pos)
            if end < 0:
                self.s_put(a(1), inp[pos:])
                st['pos'] = len(inp)
                st['state'] |= self.EOF
            else:
                self.s_put(a(1), inp[pos:end])
                st['pos'] = end + 1
            return a(0)

        def put(text):
            self.stream(a(0))['out'] += text
            return a(0)

        S, SA = STRING, STRING_ARG
        return {
            '??0' + S + 'QEAA@XZ': lambda: self.s_put(a(0), b'', fresh=True),
            '??0' + S + 'QEAA@PEBD@Z': lambda: self.s_put(a(0), self.cstr(a(1)), fresh=True),
            '??0' + S + 'QEAA@AEBV01@@Z': lambda: self.s_put(a(0), self.s_get(a(1)), fresh=True),
            '??1' + S + 'QEAA@XZ': lambda: 0,
            '??4' + S + 'QEAAAEAV01@AEBV01@@Z': lambda: self.s_put(a(0), self.s_get(a(1))),
            '??4' + S + 'QEAAAEAV01@PEBD@Z': lambda: self.s_put(a(0), self.cstr(a(1))),
            '??Y' + S + 'QEAAAEAV01@AEBV01@@Z': lambda: self.s_put(a(0), self.s_get(a(0)) + self.s_get(a(1))),
            '??A' + S + 'QEAAAEAD_K@Z': lambda: self.s_data(a(0)) + a(1),
            '?c_str@' + S + 'QEBAPEBDXZ': lambda: self.s_data(a(0)),
            '?substr@' + S + 'QEBA?AV12@_K0@Z': substr,
            '?find_first_of@' + S + 'QEBA_KD_K@Z': find_first_of_char,
            '?rfind@' + S + 'QEBA_KD_K@Z': rfind_char,
            '?swap@' + S + 'QEAAXAEAV12@@Z': swap,
            FREE.format(op='H') + 'YA?AV' + SA + '0@AEBV10@0@Z': lambda: self.s_put(a(0), self.s_get(a(1)) + self.s_get(a(2)), fresh=True),
            FREE.format(op='H') + 'YA?AV' + SA + '0@AEBV10@PEBD@Z': lambda: self.s_put(a(0), self.s_get(a(1)) + self.cstr(a(2)), fresh=True),
            FREE.format(op='8') + 'YA_NAEBV' + SA + '0@PEBD@Z': lambda: int(self.s_get(a(0)) == self.cstr(a(1))),
            FREE.format(op='8') + 'YA_NPEBDAEBV' + SA + '0@@Z': lambda: int(self.cstr(a(0)) == self.s_get(a(1))),
            FREE.format(op='M') + 'YA_NAEBV' + SA + '0@0@Z': lambda: int(self.s_get(a(0)) < self.s_get(a(1))),
            '?epsilon@?$numeric_limits@N@std@@SANXZ': lambda: f64(2.220446049250313e-16),
            '?max@?$numeric_limits@M@std@@SAMXZ': lambda: f32(3.4028234663852886e+38),
            '?max@?$numeric_limits@H@std@@SAHXZ': lambda: 0x7fffffff,
            '?min@?$numeric_limits@H@std@@SAHXZ': lambda: 0x80000000,
            '??0' + ISS + 'QEAA@AEBV' + SA + '1@H@Z': lambda: self.make_stream(a(0), self.s_get(a(1))),
            '??0' + OSS + 'QEAA@H@Z': lambda: self.make_stream(a(0)),
            '??_D' + ISS + 'QEAAXXZ': lambda: 0,
            '??_D' + OSS + 'QEAAXXZ': lambda: 0,
            '?str@' + OSS + 'QEBA?AV' + SA + '2@XZ': lambda: self.s_put(a(1), bytes(self.stream(a(0))['out']), fresh=True),
            '??$getline@DU?$char_traits@D@std@@V?$allocator@D@2@@std@@YAAEAV' + ISTREAM + '0@AEAV10@AEAV' + SA + '0@D@Z': getline,
            '??5' + ISTREAM + 'std@@QEAAAEAV01@AEAH@Z': read_number(rb'[+-]?\d+', '<i'),
            '??5' + ISTREAM + 'std@@QEAAAEAV01@AEAM@Z': read_number(rb'[+-]?(\d+\.?\d*|\.\d+)([eE][+-]?\d+)?', '<f'),
            '??6' + OSTREAM + 'std@@QEAAAEAV01@H@Z': lambda: put(str(((a(1) & 0xffffffff) ^ 0x80000000) - 0x80000000).encode()),
            '??Bios_base@std@@QEBAPEAXXZ': lambda: 0 if self.stream(a(0), True)['state'] & (self.FAIL | self.BAD) else a(0),
            '?rdstate@ios_base@std@@QEBAHXZ': lambda: self.stream(a(0), True)['state'],
        }
