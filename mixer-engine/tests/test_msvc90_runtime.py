"""tools/plugin-mapper: the Visual C++ 2008 runtime host's strings and string streams (executor/msvc90_capture.py) on a flat
memory. Neither the H910 nor the emulator is needed: the module is loaded with stand-ins for unicorn and its base classes."""
import importlib.util
import struct
import sys
import types
from pathlib import Path

import pytest

ROOT = Path(__file__).resolve().parents[2]
EXECUTOR = ROOT / 'tools/plugin-mapper/plugin_mapper/executor'


class Memory:
    """The emulator as the helpers use it: memory reads and writes, and XMM0."""

    def __init__(self):
        self.mem = bytearray(0x10000)
        self.xmm0 = None

    def mem_read(self, p, n):
        if not 0 <= p <= len(self.mem) - n:
            raise IndexError('read outside the test memory: %#x' % p)
        return bytes(self.mem[p:p + n])

    def mem_write(self, p, data):
        if not 0 <= p <= len(self.mem) - len(data):
            raise IndexError('write outside the test memory: %#x' % p)
        self.mem[p:p + len(data)] = data

    def reg_write(self, reg, value):
        self.xmm0 = value


@pytest.fixture
def m90(monkeypatch):
    x86 = types.ModuleType('unicorn.x86_const')
    x86.UC_X86_REG_XMM0 = 0
    dspap, paramobj = types.ModuleType('dspap_capture'), types.ModuleType('paramobj_capture')
    dspap.DspApCapture = type('DspApCapture', (), {})
    paramobj.ParamObjectsCapture = type('ParamObjectsCapture', (), {})
    for name, module in {'unicorn': types.ModuleType('unicorn'), 'unicorn.x86_const': x86, 'dspap_capture': dspap,
                         'paramobj_capture': paramobj}.items():
        monkeypatch.setitem(sys.modules, name, module)
    spec = importlib.util.spec_from_file_location('msvc90_test_capture', EXECUTOR / 'msvc90_capture.py')
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


@pytest.fixture
def cap(m90):
    """A Msvc90Capture without the emulator: its own methods over a flat memory, a bump allocator and the call's arguments."""
    c = m90.Msvc90Capture.__new__(m90.Msvc90Capture)
    c.uc, c.args, c.heap = Memory(), [], 0x8000
    c.arg = lambda i: c.args[i]
    c.rq = lambda p: struct.unpack('<Q', c.uc.mem_read(p, 8))[0]
    c.wq = lambda p, v: c.uc.mem_write(p, struct.pack('<Q', v))

    def reserve(size):
        p, c.heap = c.heap, c.heap + (size + 15) // 16 * 16
        return p
    c.reserve = reserve
    c.fns = c.msvcp90()
    return c


def call(cap, name, *args):
    cap.args = list(args)
    return cap.fns[name]()


def i32(cap, p):
    return struct.unpack('<i', cap.uc.mem_read(p, 4))[0]


def f32(cap, p):
    return struct.unpack('<f', cap.uc.mem_read(p, 4))[0]


def istream(m90, cap, text, at):
    cap.s_put(0x300, text, fresh=True)
    call(cap, '??0' + m90.ISS + 'QEAA@AEBV' + m90.STRING_ARG + '1@H@Z', at, 0x300, 1)
    return at


def state(m90, cap, stream):
    return call(cap, '?rdstate@ios_base@std@@QEBAHXZ', stream + m90.Msvc90Capture.IOS)


def test_a_string_is_kept_in_its_pointer_form_and_grows(cap):
    s = 0x100
    cap.s_put(s, b'Pitch', fresh=True)
    assert cap.s_get(s) == b'Pitch' and cap.rq(s + 0x18) == 5 and cap.rq(s + 0x20) == 16
    assert cap.cstr(cap.s_data(s)) == b'Pitch'
    first = cap.rq(s + 8)
    cap.s_put(s, b'Pitch Control')                      # fits its capacity: the same buffer
    assert cap.rq(s + 8) == first and cap.s_get(s) == b'Pitch Control'
    cap.s_put(s, b'x' * 40)                             # past it: a new buffer
    assert cap.rq(s + 8) != first and cap.rq(s + 0x20) == 40 and cap.s_get(s) == b'x' * 40
    cap.wq(s + 0x18, 0x200000)
    with pytest.raises(ValueError, match='implausible'):
        cap.s_get(s)


def test_substr_rfind_and_find_first_of(m90, cap):
    S, NPOS = m90.STRING, m90.NPOS
    substr, rfind, first_of = '?substr@' + S + 'QEBA?AV12@_K0@Z', '?rfind@' + S + 'QEBA_KD_K@Z', '?find_first_of@' + S + 'QEBA_KD_K@Z'
    text, out = 0x100, 0x200
    cap.s_put(text, b'a/b/c', fresh=True)
    call(cap, substr, text, out, 2, NPOS)
    assert cap.s_get(out) == b'b/c'
    call(cap, substr, text, out, 0, 1)
    assert cap.s_get(out) == b'a'
    call(cap, substr, text, out, 5, NPOS)
    assert cap.s_get(out) == b''
    with pytest.raises(ValueError, match='out of range'):
        call(cap, substr, text, out, 6, NPOS)
    assert call(cap, rfind, text, ord('/'), NPOS) == 3
    assert call(cap, rfind, text, ord('/'), 2) == 1
    assert call(cap, rfind, text, ord('x'), NPOS) == NPOS
    assert call(cap, first_of, text, ord('/'), 2) == 3
    assert call(cap, first_of, text, ord('/'), 9) == NPOS


def test_getline_reads_up_to_each_delimiter(m90, cap):
    getline = ('??$getline@DU?$char_traits@D@std@@V?$allocator@D@2@@std@@YAAEAV' + m90.ISTREAM + '0@AEAV10@AEAV'
               + m90.STRING_ARG + '0@D@Z')
    C, s, line = m90.Msvc90Capture, istream(m90, cap, b'one\ntwo', at=0x400), 0x500
    call(cap, getline, s, line, ord('\n'))
    assert cap.s_get(line) == b'one' and state(m90, cap, s) == 0
    call(cap, getline, s, line, ord('\n'))
    assert cap.s_get(line) == b'two' and state(m90, cap, s) == C.EOF
    call(cap, getline, s, line, ord('\n'))
    assert cap.s_get(line) == b'' and state(m90, cap, s) == C.EOF | C.FAIL


def test_numbers_are_read_as_the_runtime_reads_them(m90, cap):
    read_int, read_float = ('??5' + m90.ISTREAM + 'std@@QEAAAEAV01@AEA' + t + '@Z' for t in 'HM')
    good = '??Bios_base@std@@QEBAPEAXXZ'
    C, value = m90.Msvc90Capture, 0x600
    s = istream(m90, cap, b'  12 -7 x', at=0x400)
    call(cap, read_int, s, value)
    assert i32(cap, value) == 12
    call(cap, read_int, s, value)
    assert i32(cap, value) == -7 and state(m90, cap, s) == 0 and call(cap, good, s + C.IOS)
    call(cap, read_int, s, value)                       # no number: failbit, the variable as it was
    assert i32(cap, value) == -7 and state(m90, cap, s) == C.FAIL and not call(cap, good, s + C.IOS)
    s = istream(m90, cap, b'99999999999 5', at=0x700)
    call(cap, read_int, s, value)                       # outside int: the same
    assert i32(cap, value) == -7 and state(m90, cap, s) == C.FAIL
    s = istream(m90, cap, b'1.5e3', at=0x800)
    call(cap, read_float, s, value)
    assert f32(cap, value) == 1500.0 and state(m90, cap, s) == C.EOF
    s = istream(m90, cap, b'1e39', at=0x900)
    call(cap, read_float, s, value)                     # outside float: the same
    assert f32(cap, value) == 1500.0 and state(m90, cap, s) == C.FAIL | C.EOF


def test_an_ostringstream_and_the_end_of_a_stream(m90, cap):
    OSS, out = m90.OSS, 0x500
    s = call(cap, '??0' + OSS + 'QEAA@H@Z', 0x400, 2)
    call(cap, '??6' + m90.OSTREAM + 'std@@QEAAAEAV01@H@Z', s, 0xfffffffb)     # -5 as the register holds it
    call(cap, '??6' + m90.OSTREAM + 'std@@QEAAAEAV01@H@Z', s, 12)
    call(cap, '?str@' + OSS + 'QEBA?AV' + m90.STRING_ARG + '2@XZ', s, out)
    assert cap.s_get(out) == b'-512'
    call(cap, '??_D' + OSS + 'QEAAXXZ', s)
    with pytest.raises(ValueError, match='did not construct'):
        call(cap, '?str@' + OSS + 'QEBA?AV' + m90.STRING_ARG + '2@XZ', s, out)
    with pytest.raises(ValueError, match='did not construct'):
        call(cap, '??_D' + OSS + 'QEAAXXZ', s)
    call(cap, '??0' + OSS + 'QEAA@H@Z', s, 2)            # constructed again: a fresh stream
    call(cap, '?str@' + OSS + 'QEBA?AV' + m90.STRING_ARG + '2@XZ', s, out)
    assert cap.s_get(out) == b''
