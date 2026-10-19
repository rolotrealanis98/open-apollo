"""Parsed PE images, once per process: the daemon starts a plug-in's emulator for every load, and
re-parsing the same DLLs (their exception directories above all) was most of that time.
Callers only read the returned objects; a changed file (size or mtime) is parsed again."""
import os
import threading

import pefile

_CACHE = {}
_LOCK = threading.Lock()


def parsed(path, directories=None, fast_load=True):
    """pefile.PE(path, fast_load=fast_load) with these data directories parsed (all when
    fast_load is False). Shared: never mutate the result."""
    path = os.fspath(path)
    st = os.stat(path)
    key = (path, st.st_size, st.st_mtime_ns, fast_load, tuple(directories or ()))
    with _LOCK:
        pe = _CACHE.get(key)
    if pe is None:
        pe = pefile.PE(path, fast_load=fast_load)
        if fast_load and directories:
            pe.parse_data_directories(directories=list(directories))
        with _LOCK:
            pe = _CACHE.setdefault(key, pe)
    return pe


def from_bytes(sha256, raw, relocate_to=None):
    """(pefile.PE(data=raw) relocated to `relocate_to` if given, its memory-mapped image), once per
    process for these bytes (keyed by their SHA-256, which callers already check). Shared."""
    key = ('bytes', sha256, relocate_to)
    with _LOCK:
        hit = _CACHE.get(key)
    if hit is None:
        pe = pefile.PE(data=raw)
        if relocate_to is not None:
            pe.relocate_image(relocate_to)
        hit = (pe, pe.get_memory_mapped_image())
        with _LOCK:
            hit = _CACHE.setdefault(key, hit)
    return hit
