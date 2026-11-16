"""tools/plugin-mapper: the Softube-engine family, with synthetic records and code only (no UA file)."""
import hashlib
import importlib.util
import struct
import sys
import types
from pathlib import Path

import pytest

import native_carrier

ROOT = Path(__file__).resolve().parents[2]
PKG = ROOT / 'tools/plugin-mapper/plugin_mapper'


def load(name, path):
    spec = importlib.util.spec_from_file_location(name, path)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


descriptor = load('softube_test_descriptor', PKG / 'carrier/descriptor.py')


def record(ident, word2, functional, payload_words, seed=1):
    """A synthetic Bill record: header, then `payload_words` words of a counter, the last `functional` the pool."""
    payload = struct.pack('<%dI' % payload_words, *[(seed * 2654435761 + i * 40503) & 0xffffffff for i in range(payload_words)])
    return struct.pack('<5I', 0x6c6c6942, ident, word2, len(payload), functional) + payload


def test_the_host_fills_the_pool_with_minstd_from_the_ids_complement():
    # What a Windows host uploaded for the Tube-Tech CL 1B's data modules before overwriting them (a UAD-2 Satellite
    # USB capture): the generator's output, not module bytes.
    def pool(ident, functional, n):
        rec = record(ident, 0x00300001, functional, functional + 8)
        out = descriptor.host_filled(rec)
        assert out[:8] == rec[:8] and out[12:52] == rec[12:52]           # header and the 8 words before the pool
        assert struct.unpack_from('<I', out, 8)[0] == 0x00000001         # word 2 less its rate
        return [struct.unpack_from('>I', out, 52 + 4 * i)[0] for i in range(n)]
    assert pool(0x27a, 1522, 5) == [0xfffffd85, 0x7e2dc268, 0x1654b1f8, 0x2c01aa6d, 0x4e167fb6]
    assert pool(0x27b, 2, 2) == [0xfffffd84, 0x7e2d05d9]
    with pytest.raises(ValueError):
        descriptor.host_filled(record(0x275, 0x02000000, 30, 60))       # a code module


def store_with(tmp_path, *records):
    for rec in records:
        (tmp_path / (hashlib.sha256(rec).hexdigest() + '.bill')).write_bytes(rec)
    return tmp_path


def test_a_module_the_plug_in_filled_is_the_stored_module_and_an_overwrite(tmp_path):
    stored = record(0x27a, 0x00000001, 10, 18)
    other = record(0x27a, 0x00000001, 10, 18, seed=2)                    # same layout, other words before the pool
    store = store_with(tmp_path, stored, other, record(0x27b, 0x00000002, 2, 10))
    table = list(range(100, 110))
    in_memory = stored[:8] + struct.pack('<I', 0x00300001) + stored[12:52] + struct.pack('<10I', *table)
    r = descriptor.pool_patched_resource(store, in_memory)
    assert r['sha256'] == hashlib.sha256(stored).hexdigest() and r['module_store_verified']
    assert (r['id'], r['kind'], r['architecture'], r['words']) == (0x27a, 1, 0, 10)
    assert r['host_overwrite']['words'] == table and r['host_overwrite']['upload'] == 'host_filled'
    assert r['pool_patched']['in_memory_word2'] == '0x300001'
    store_with(tmp_path, stored[:52] + struct.pack('<10I', *range(10)))  # a second store module of its layout
    with pytest.raises(ValueError, match='2 store modules'):
        descriptor.pool_patched_resource(store, in_memory)
    with pytest.raises(ValueError, match='0 store modules'):
        descriptor.pool_patched_resource(store, in_memory[:20] + b'\xff' + in_memory[21:])


def test_the_native_carrier_refuses_a_module_it_would_upload_as_stored():
    with pytest.raises(ValueError, match='host-filled'):
        native_carrier.resource(dict(id=0x27a, architecture=0, sha256='0' * 64, host_overwrite=dict(words=[1, 2])))


def test_softube_plug_ins_take_the_softube_family(monkeypatch):
    monkeypatch.setattr(sys, 'path', sys.path[:])                       # structural_family adds the discovery directory
    spec = importlib.util.spec_from_file_location('softube_test_mapper', PKG / '__main__.py')
    mapper = importlib.util.module_from_spec(spec)
    monkeypatch.setitem(sys.modules, 'softube_test_mapper', mapper)
    spec.loader.exec_module(mapper)
    fake_classify = types.SimpleNamespace(classify=lambda img, vt: ('Softube', ['CSoftubeEffectUAD']),
                                          cols=lambda img: None, vtables=lambda img, cols: None)
    fake_profiles = types.SimpleNamespace(Image=lambda path: None)
    monkeypatch.setitem(sys.modules, 'classify_vst2_iuadplug', fake_classify)
    monkeypatch.setitem(sys.modules, 'dspaudioprocessor_profiles', fake_profiles)
    assert mapper.structural_family('x.dll') == 'softube'
    assert 'softube' in mapper.GENERATORS


class Code:
    """The `img` the discovery functions read: an image of hand-assembled x86-64 at 0x180001000."""
    base = 0x180000000

    def __init__(self, code):
        from capstone import CS_ARCH_X86, CS_MODE_64, Cs
        self.im = bytes(0x1000) + code + bytes(0x400)
        self.cs = Cs(CS_ARCH_X86, CS_MODE_64)
        self.cs.detail = True


VA = 0x180001000


@pytest.fixture(scope='module')
def softube():
    pytest.importorskip('capstone')
    pytest.importorskip('pefile')
    sys.path.insert(0, str(PKG / 'discovery'))
    try:
        return load('softube_test_profiles', PKG / 'discovery/softube_profiles.py')
    finally:
        sys.path.remove(str(PKG / 'discovery'))


def test_the_descriptor_getter_and_its_channel_fields_are_found_from_the_code(softube):
    getter = Code(bytes.fromhex(
        '4c8bf1'                # mov r14, rcx
        '4439a930feffff'        # cmp [rcx - 0x1d0], r13d      inputs
        'ff9080010000'          # call [rax + 0x180]            isInputConnected
        '4539ae34feffff'        # cmp [r14 - 0x1cc], r13d      outputs
        'ff9088010000'          # call [rax + 0x188]            isOutputConnected
        '41b8e80a0000'          # mov r8d, 0xae8                the AllocInfo's size
        'c3'))
    assert softube.getter_fields(getter, VA) == dict(num_inputs_disp=-0x1d0, num_outputs_disp=-0x1cc)
    assert softube.fills_alloc_info(getter, VA)
    with pytest.raises(ValueError, match='channel loops'):
        softube.getter_fields(Code(bytes.fromhex('4439a930feffffc3')), VA)


def test_only_the_handlers_fields_and_the_effects_calls_count(softube):
    getter = Code(bytes.fromhex(
        '4c8bf1'                # mov r14, rcx
        '395df0'                # cmp [rbp - 0x10], ebx         a local
        'ff1500100000'          # call [rip + 0x1000]           an import
        '4539ae30feffff'        # cmp [r14 - 0x1d0], r13d      inputs
        'ff5030'                # call [rax + 0x30]             another slot
        'ff9080010000'          # call [rax + 0x180]            isInputConnected
        '4539ae34feffff'        # cmp [r14 - 0x1cc], r13d      outputs
        'ff9088010000'          # call [rax + 0x188]            isOutputConnected
        'c3'))
    assert softube.getter_fields(getter, VA) == dict(num_inputs_disp=-0x1d0, num_outputs_disp=-0x1cc)
    lost = Code(bytes.fromhex(
        'ff9080010000'          # call [rax + 0x180]            rcx is no longer the handler
        '399130feffff'          # cmp [rcx - 0x1d0], edx
        '399134feffff'          # cmp [rcx - 0x1cc], edx
        'ff9088010000'          # call [rax + 0x188]
        'c3'))
    with pytest.raises(ValueError, match='channel loops'):
        softube.getter_fields(lost, VA)


def test_a_getter_that_returns_early_still_fills_the_alloc_info(softube):
    assert softube.fills_alloc_info(Code(bytes.fromhex(
        '4885d2'                # test rdx, rdx
        '7501'                  # jne +1
        'c3'                    # ret                           no AllocInfo
        '41b8e80a0000'          # mov r8d, 0xae8
        'c3cc')), VA)
    assert not softube.fills_alloc_info(Code(bytes.fromhex('c3cc41b8e80a0000')), VA)    # past the padding: another's


def test_a_parameters_domain_and_default(softube):
    def prop(norm, steps=None):
        p = dict(Name=dict(value='Mode'), NormalizedValue=dict(value=norm), StringValue=dict(value=' On '))
        if steps is not None:
            p['StepValue'] = dict(values=steps)
        return p
    assert softube.parameter(prop(0.5, ['A', 'B', 'C'])) == dict(name='Mode', maximum=2, default=1, steps=3, exact=True,
                                                                captured_text='On')
    assert softube.parameter(prop(0.25)) == dict(name='Mode', maximum=32768, default=8192, steps=0, exact=True,
                                                 captured_text='On')
    assert softube.parameter(prop(0.1))['default'] == 3277 and not softube.parameter(prop(0.1))['exact']
    with pytest.raises(ValueError, match='one step'):
        softube.parameter(prop(0.0, ['Only']))
    with pytest.raises(ValueError, match='outside 0..1'):
        softube.parameter(prop(1.5, ['A', 'B']))
