"""Long control plans split into service-sized chunks at whole WRITE-block boundaries."""
import pytest

from plugin_native import _write_chunks


def block(row, word, values):
    return (0x00140000 | (3 + len(values)), row << 24 | word, 0, *values)


def test_short_plan_is_one_chunk():
    words = block(10, 0, [1, 2, 3])
    assert _write_chunks(words) == (words,)


def test_long_plan_splits_without_cutting_a_block():
    blocks = [block(10, i * 100, list(range(97))) for i in range(25)]  # 25 x 100 words
    words = tuple(w for b in blocks for w in b)
    chunks = _write_chunks(words)
    assert all(len(c) <= 1016 for c in chunks)
    assert sum(chunks, ()) == words
    for c in chunks:  # every chunk starts on a block header and holds whole blocks
        i = 0
        while i < len(c):
            assert c[i] >> 16 == 0x14
            i += c[i] & 0xffff
        assert i == len(c)


def test_malformed_long_plan_refuses():
    with pytest.raises(ValueError):
        _write_chunks((0x00150004, 0, 0, 0) * 300)
