"""Made-up stand-ins for the DSP modules of the Solo USB init (no UA bytes).

The real modules are UA's code: the setup wizard or `oa-assets firmware`
extracts them from the user's own UAD install into
/lib/firmware/universal-audio/dsp.  The stand-ins written here have the ids,
lengths and w4 words the shipped facts (tools/uausb/solo-usb-init.json)
name and a valid on-disk Bill header, around a made-up payload, so the
generator places and frames them exactly as it does the real ones.  With a
zero payload the generated stream is the captured one with its code
payloads zeroed: the facts' code_blanked_sha256.

The Bill magic is assembled at run time, so this file holds no record.
"""

import copy
import hashlib
import os
import struct
import sys
from pathlib import Path

from conftest import ENGINE_DIR

TOOLS = ENGINE_DIR.parent / "tools"
if str(TOOLS) not in sys.path:
    sys.path.append(str(TOOLS))

from uausb import dspcmd as dc  # noqa: E402
from uausb import soloinit as si  # noqa: E402

FACTS = si.load_facts()
MODULE_IDS = [int(m["id"], 16) for m in FACTS["modules"]]
MAGIC = struct.pack("<I", dc.BILL_MAGIC)

# The user's extracted modules, for the byte-identity test
REAL_DSP_DIR = Path(os.environ.get("OA_DSP_DIR") or si.DEFAULT_DSP_DIR)


def real_modules_present():
    return all((REAL_DSP_DIR / si.module_file(m)).is_file() for m in MODULE_IDS)


def payload(size, seed=None):
    if seed is None:
        return bytes(size)
    return bytes((seed + 29 * i + (i >> 8)) & 0xFF for i in range(size))


def fake_record(module, length, w4, seed=None, w2=si.BILL_W2_DISK):
    """An on-disk Bill record: magic, id, word 2, payload size, w4, payload."""
    size = length - 4 * si.BILL_HDR_WORDS
    return MAGIC + struct.pack("<IIII", module, w2, size, w4) + payload(size, seed)


def write_modules(directory, seed=None, facts=FACTS):
    """<id>.bill stand-ins for every module the facts name, in directory.

    Returns (facts naming their SHA-256, {module id: record}).  With a zero
    payload (seed None) the facts' stream hash becomes the code-blanked
    capture's, which is what these modules rebuild.
    """
    directory = Path(directory)
    directory.mkdir(parents=True, exist_ok=True)
    out = copy.deepcopy(facts)
    records = {}
    for m in out["modules"]:
        module = int(m["id"], 16)
        rec = fake_record(module, m["length"], m["w4"], seed)
        (directory / si.module_file(module)).write_bytes(rec)
        m["sha256"] = hashlib.sha256(rec).hexdigest()
        records[module] = rec
    if seed is None:
        out["stream"]["sha256"] = facts["stream"]["code_blanked_sha256"]
    return out, records


def container_sha256(urbs):
    return hashlib.sha256(dc.build_urb_container(urbs)).hexdigest()
