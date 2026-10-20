"""pe_cache keeps only the most recently used parsed DLLs (a worker kept every plug-in's)."""
import sys
import types
from pathlib import Path

sys.path.append(str(Path(__file__).resolve().parents[2] / 'tools/plugin-mapper/plugin_mapper/executor'))
try:
    import pefile  # noqa: F401
except ImportError:  # CI has no pefile; these tests never parse a DLL
    sys.modules['pefile'] = types.ModuleType('pefile')
import pe_cache  # noqa: E402


class FakePE:
    def __init__(self, path=None, fast_load=True, data=None):
        self.path = path

    def parse_data_directories(self, directories):
        pass


def test_the_cache_is_bounded_and_keeps_what_is_in_use(monkeypatch, tmp_path):
    monkeypatch.setattr(pe_cache.pefile, 'PE', FakePE, raising=False)
    monkeypatch.setattr(pe_cache, '_CACHE', type(pe_cache._CACHE)())
    sdk = tmp_path / 'UAD2SDK.dll'
    sdk.write_bytes(b'sdk')
    first = pe_cache.parsed(sdk)
    for n in range(3 * pe_cache.MAX_ENTRIES):         # a library of plug-ins, one after another
        dll = tmp_path / f'plugin{n}.dll'
        dll.write_bytes(b'x')
        pe_cache.parsed(dll, [9])
        assert pe_cache.parsed(sdk) is first          # every emulator start uses the SDK: kept
    assert len(pe_cache._CACHE) == pe_cache.MAX_ENTRIES
    assert pe_cache.parsed(tmp_path / 'plugin0.dll', [9]) is not None   # an evicted one: parsed again
