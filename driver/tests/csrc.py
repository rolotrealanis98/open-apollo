"""Helpers for the driver host tests.

The tests take functions and macros out of the driver sources as they are,
put them next to userspace stand-ins for the kernel API they use, build the
result with the host C compiler and run it.  No module is loaded and no
hardware is touched; what they check is the logic of the extracted code.
"""
from pathlib import Path
import shutil
import subprocess
import tempfile

DRIVER = Path(__file__).resolve().parents[1]
CC = shutil.which('cc') or shutil.which('gcc') or shutil.which('clang')


def source(name):
    """Text of driver/<name>."""
    return (DRIVER / name).read_text()


def extract(text, signature):
    """The definition that starts with `signature`, up to its closing brace.

    A prototype (a `;` before the first `{`) is skipped and the next
    occurrence is used.
    """
    start = text.index(signature)
    while ';' in text[start:text.index('{', start)]:
        start = text.index(signature, start + len(signature))
    end = text.index('{', start) + 1
    depth = 1
    while depth:
        depth += (text[end] == '{') - (text[end] == '}')
        end += 1
    return text[start:end]


def between(text, first, last):
    """text from `first` up to (not including) the next `last`."""
    start = text.index(first)
    return text[start:text.index(last, start)]


def build_and_run(code, *, threads=False, extra=()):
    """Compile `code` with -Wall -Wextra -Werror, run it, raise on failure."""
    assert CC, 'no host C compiler'
    with tempfile.TemporaryDirectory() as tmp:
        src = Path(tmp) / 't.c'
        exe = Path(tmp) / 't'
        src.write_text(code)
        cmd = [CC, '-std=gnu11', '-D_DEFAULT_SOURCE', '-Wall', '-Wextra',
               '-Werror', '-Wno-unused-function', *extra, str(src),
               '-o', str(exe)]
        if threads:
            cmd.insert(1, '-pthread')
        built = subprocess.run(cmd, capture_output=True, text=True,
                               timeout=60)
        if built.returncode:
            raise AssertionError('compile failed:\n' + built.stderr)
        ran = subprocess.run([str(exe)], capture_output=True, text=True,
                             timeout=30)
        if ran.returncode:
            raise AssertionError('test program failed (%d):\n%s%s' %
                                 (ran.returncode, ran.stdout, ran.stderr))
        return ran.stdout
