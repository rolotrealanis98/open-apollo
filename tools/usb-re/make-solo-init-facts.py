#!/usr/bin/env python3
"""Write the Solo USB init facts (tools/uausb/solo-usb-init.json) from a capture.

For maintainers.  CAPTURE is a Windows capture of an Apollo Solo USB's DSP
bring-up in the URB container format (u32 frame count, then u32 length +
bytes per bulk OUT frame, little-endian; the bulk OUT frames only, the
0xDC stream from the first strip stop to the last SYNCH enable, without
the connect and rate-ack frames before it).  The facts hold
everything tools/uausb/soloinit.py needs from it except UA's code: the
strip parameter blocks, the frame sizes and first sequence number, the ids,
lengths, w4 words and SHA-256 of the DSP modules it loads, and the SHA-256
of the whole stream (and of the stream with the code payloads zeroed).

The capture holds UA's DSP modules: keep it out of the repository.  This
only reads files; it talks to no device.  It refuses a capture the
generator does not rebuild byte for byte.

Usage: python3 tools/usb-re/make-solo-init-facts.py CAPTURE OUT [--rate 48000]
"""
import argparse
import json
import os
import sys

_tools = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
if _tools not in sys.path:
    sys.path.append(_tools)

from uausb import dspcmd, soloinit  # noqa: E402

PINS = os.path.join(os.path.dirname(_tools),
                    "console-rs", "crates", "oa-assets", "data",
                    "firmware-pins.json")


def dumps(obj, indent=0):
    """JSON with lists of numbers and strings kept on one line."""
    pad = "  " * indent
    if isinstance(obj, dict):
        items = [f'{pad}  {json.dumps(k)}: {dumps(v, indent + 1).lstrip()}'
                 for k, v in obj.items()]
        return pad + "{\n" + ",\n".join(items) + "\n" + pad + "}"
    if isinstance(obj, list) and obj and all(isinstance(v, (dict, list))
                                             for v in obj):
        items = [dumps(v, indent + 1) for v in obj]
        return pad + "[\n" + ",\n".join(items) + "\n" + pad + "]"
    return pad + json.dumps(obj)


def pinned_hashes(path=PINS):
    try:
        with open(path) as f:
            doc = json.load(f)
    except (OSError, ValueError):
        return None
    return {k["sha256"].lower() for m in doc.get("dsp_modules", [])
            for k in m.get("known", [])}


def main(argv=None):
    ap = argparse.ArgumentParser(description=__doc__.split("\n")[0])
    ap.add_argument("capture", help="the captured init stream (URB container)")
    ap.add_argument("out", help="where to write the facts JSON")
    ap.add_argument("--rate", type=int, default=soloinit.CAPTURE_RATE,
                    help="the captured session's sample rate (default 48000)")
    args = ap.parse_args(argv)

    try:
        stream = dspcmd.InitStream.from_file(args.capture)
        facts = soloinit.make_facts(stream, rate=args.rate)
    except (OSError, dspcmd.DspCommandError, soloinit.TemplateError) as e:
        print(f"{args.capture}: {e}", file=sys.stderr)
        return 1

    pins = pinned_hashes()
    for m in facts["modules"]:
        note = ""
        if pins is not None and m["sha256"] not in pins:
            note = "  (not in firmware-pins.json: oa-assets will not extract it)"
        print(f"module {m['id']}: {m['length']} bytes, w4 {m['w4']}, "
              f"sha256 {m['sha256'][:16]}...{note}")
    s = facts["stream"]
    print(f"stream: {s['frames']} frames, {s['commands']} commands, "
          f"{s['bytes']} bytes, sha256 {s['sha256'][:16]}...")

    text = dumps(facts) + "\n"
    with open(args.out, "w") as f:
        f.write(text)
    print(f"wrote {args.out} ({len(text)} bytes)")
    return 0


if __name__ == "__main__":
    sys.exit(main())
