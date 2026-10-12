#!/usr/bin/env -S python3 -I
"""Compare where text ink lands in a reference Console shot and in our render, label by label.

  tools/align/text_diff.py REF.png OURS.png [--crop x,y,w,h] [--draws OURS.draws.json] [--out diff.json]
                           [--match TEXT] [--crops PREFIX] [--min-contrast 60]

For every text run our renderer drew (from OURS's `.draws.json`, written by `uaconsole --render`)
inside the crop, the ink of that label is found in both images: the pixels in the label's box
(grown by --grow pt, cut to its clip) whose colour lies at least half way from the local
background towards the text colour. Each row reports the ink box in points for REF and OURS and
d = OURS - REF for left, top, right, bottom, width and height, plus the baseline (ink bottom of
labels without descenders) and the ink mass ratio (bold/weight hint).

The summary groups rows by (family, size, bold): median dh, dw, height ratio and baseline offset.
Those medians calibrate font size and vertical placement. Labels whose text differs between
Console and us (other values, other names) show up as outliers. Use --match to select labels.

REF, OURS and --crops images contain UA art: keep them out of git.
"""
import os
import sys

TOOLS = os.environ.get("CONSOLELAB_TOOLS") or os.path.expanduser("~/github/ConsoleLab/tools")
sys.path.insert(0, TOOLS)
try:
    import skinart  # noqa: E402
except ImportError:
    sys.exit(f"text_diff: ConsoleLab tools not found at {TOOLS} (set CONSOLELAB_TOOLS)")

skinart.bootstrap(__file__)

import argparse  # noqa: E402
import json  # noqa: E402
import re  # noqa: E402

import cv2  # noqa: E402
import numpy as np  # noqa: E402

FORMAT = "text-diff/1"
DESCENDERS = set("gjpqy,;()[]{}|/_@$Q")


def parse_rect(s):
    v = [float(x) for x in s.split(",")]
    if len(v) != 4:
        raise argparse.ArgumentTypeError("expected x,y,w,h")
    return v


def intersect(a, b):
    if a is None or b is None:
        return a if b is None else b
    x0, y0 = max(a[0], b[0]), max(a[1], b[1])
    x1, y1 = min(a[0] + a[2], b[0] + b[2]), min(a[1] + a[3], b[1] + b[3])
    return [x0, y0, x1 - x0, y1 - y0] if x1 > x0 and y1 > y0 else None


def load(path):
    im = cv2.imread(path, cv2.IMREAD_UNCHANGED)
    if im is None:
        sys.exit(f"cannot read {path}")
    if im.ndim == 2:
        im = cv2.cvtColor(im, cv2.COLOR_GRAY2BGR)
    if im.shape[2] == 4:
        a = im[:, :, 3:4].astype(np.float32) / 255
        im = (im[:, :, :3].astype(np.float32) * a + 0.5).astype(np.uint8)
    return im


def ink_box(img, region_px, box_px, color_bgr, frac):
    """Ink bbox (x0, y0, x1, y1 px, exclusive), mass and background of the text colour in region.
    The background is the median of the label's own box (text covers a minority of it); ink lies
    at least `frac` of the way from it towards the text colour and close to that colour axis."""
    x0, y0, x1, y1 = region_px
    win = img[y0:y1, x0:x1].astype(np.float32)
    if win.size == 0:
        return None
    bx0, by0, bx1, by1 = box_px
    own = img[max(by0, 0):by1, max(bx0, 0):bx1].reshape(-1, 3).astype(np.float32)
    if not len(own):
        return None
    bg = np.median(own, axis=0)
    c = np.array(color_bgr, np.float32)
    v = c - bg
    n2 = float((v * v).sum())
    if n2 < 1:
        return None
    t = ((win - bg) * v).sum(axis=2) / n2
    off = np.sqrt((((win - bg) - t[:, :, None] * v) ** 2).sum(axis=2))
    ink = (t >= frac) & (off <= 0.35 * np.sqrt(n2))
    # Glyphs do not touch the grown search border; neighbouring panels and edges do.
    n, lab, stats, _ = cv2.connectedComponentsWithStats(ink.astype(np.uint8), connectivity=8)
    h, w = ink.shape
    for i in range(1, n):
        x, y, cw, ch = stats[i, :4]
        if x == 0 or y == 0 or x + cw == w or y + ch == h:
            ink[lab == i] = False
    if not ink.any():
        return dict(bg=bg, contrast=float(np.sqrt(n2)), box=None, mass=0.0)
    ys, xs = np.nonzero(ink)
    mass = float(np.clip(t, 0, 1).sum())
    return dict(bg=bg, contrast=float(np.sqrt(n2)), box=(x0 + xs.min(), y0 + ys.min(), x0 + xs.max() + 1, y0 + ys.max() + 1),
                mass=mass)


def main():
    ap = argparse.ArgumentParser(description=__doc__.split("\n\n")[0],
                                 formatter_class=argparse.RawDescriptionHelpFormatter, epilog=__doc__.split("\n\n", 1)[1])
    ap.add_argument("ref")
    ap.add_argument("ours")
    ap.add_argument("--crop", type=parse_rect, help="region x,y,w,h in pt")
    ap.add_argument("--draws", help="draws sidecar (default OURS.draws.json)")
    ap.add_argument("--out", help="JSON output (default OURS.text.json)")
    ap.add_argument("--ref-scale", type=float, help="REF px per pt (default: REF.json 'scale', else 2)")
    ap.add_argument("--grow", type=float, default=3.0, help="search the label box grown by this many pt (default 3)")
    ap.add_argument("--ink", type=float, default=0.5, help="ink = at least this fraction from background to text colour")
    ap.add_argument("--min-contrast", type=float, default=60.0, help="skip labels whose colour is this close to the background")
    ap.add_argument("--match", help="regex: only labels whose text matches")
    ap.add_argument("--crops", help="write PREFIX.png: REF over OURS crops per label (UA art: keep out of git)")
    ap.add_argument("--limit", type=int, default=0, help="table rows to print (0 = all)")
    args = ap.parse_args()

    dpath = args.draws or os.path.splitext(args.ours)[0] + ".draws.json"
    with open(dpath) as f:
        draws = json.load(f)
    rs = args.ref_scale
    if rs is None:
        side = os.path.splitext(args.ref)[0] + ".json"
        try:
            with open(side) as f:
                rs = float(json.load(f).get("scale", 2.0))
        except (OSError, ValueError, AttributeError):
            rs = 2.0
    os_ = float(draws.get("scale", rs))
    ref, ours = load(args.ref), load(args.ours)
    pat = re.compile(args.match) if args.match else None

    rows, crops = [], []
    for t in draws.get("texts", []):
        text = t["text"].strip()
        if not text or (pat and not pat.search(t["text"])):
            continue
        r = t["rect"]
        g = args.grow
        region = intersect([r[0] - g, r[1] - g, r[2] + 2 * g, r[3] + 2 * g], t.get("clip"))
        region = intersect(region, args.crop) if args.crop else region
        if not region or region[2] < 2 or region[3] < 2:
            continue
        col = t["color"]
        if col[3] < 0.3:
            continue
        bgr = [col[2] * 255, col[1] * 255, col[0] * 255]

        def px(img, s):
            x0, y0 = int(np.floor(region[0] * s)), int(np.floor(region[1] * s))
            x1, y1 = int(np.ceil((region[0] + region[2]) * s)), int(np.ceil((region[1] + region[3]) * s))
            return max(0, x0), max(0, y0), min(img.shape[1], x1), min(img.shape[0], y1)
        own = intersect(intersect(r, t.get("clip")), args.crop) if args.crop else intersect(r, t.get("clip"))
        if not own:
            continue

        def pxr(rect, img, s):
            return (int(np.floor(rect[0] * s)), int(np.floor(rect[1] * s)),
                    min(img.shape[1], int(np.ceil((rect[0] + rect[2]) * s))), min(img.shape[0], int(np.ceil((rect[1] + rect[3]) * s))))
        a = ink_box(ref, px(ref, rs), pxr(own, ref, rs), bgr, args.ink)
        b = ink_box(ours, px(ours, os_), pxr(own, ours, os_), bgr, args.ink)
        if not a or not b or a["box"] is None or b["box"] is None:
            continue
        if b["contrast"] < args.min_contrast:
            continue
        ra = [round(v / rs, 2) for v in a["box"]]
        rb = [round(v / os_, 2) for v in b["box"]]
        d = [round(rb[k] - ra[k], 2) for k in range(4)]
        hr, hb = ra[3] - ra[1], rb[3] - rb[1]
        wr, wb = ra[2] - ra[0], rb[2] - rb[0]
        base = not any(ch in DESCENDERS for ch in text) and "\n" not in text
        row = {
            "text": t["text"], "family": t["family"], "size": t["size"], "bold": t["bold"], "align": t["align"],
            "rect": r, "ref_ink": ra, "ours_ink": rb,
            "dl": d[0], "dt": d[1], "dr": d[2], "db": d[3],
            "dw": round(wb - wr, 2), "dh": round(hb - hr, 2),
            "h_ratio": round(hb / hr, 3) if hr > 0 else None,
            "w_ratio": round(wb / wr, 3) if wr > 0 else None,
            "baseline": d[3] if base else None,
            "mass_ratio": round(b["mass"] / (a["mass"] * (os_ / rs) ** 2), 3) if a["mass"] else None,
        }
        rows.append(row)
        if args.crops:
            x0, y0, x1, y1 = px(ref, rs)
            cr = ref[y0:y1, x0:x1]
            x0, y0, x1, y1 = px(ours, os_)
            co = ours[y0:y1, x0:x1]
            if cr.size and co.size:
                h = max(cr.shape[0], co.shape[0])
                w = cr.shape[1] + co.shape[1] + 4
                tile = np.zeros((h, w, 3), np.uint8)
                tile[:cr.shape[0], :cr.shape[1]] = cr
                tile[:co.shape[0], cr.shape[1] + 4:] = co
                crops.append(tile)

    rows.sort(key=lambda r: (round(r["rect"][1]), r["rect"][0]))
    shown = rows[: args.limit] if args.limit else rows
    print(f"{'text':24s} {'font':18s} {'ref ink l,t,r,b (pt)':27s} {'dl':>6s} {'dt':>6s} {'dr':>6s} {'db':>6s} {'dw':>6s} {'dh':>6s} base  mass")
    for r in shown:
        font = f"{r['family'][:10]} {r['size']:g}{'b' if r['bold'] else ''}"
        print(f"{r['text'].replace(chr(10), ' ')[:24]:24s} {font:18s} {'{:6.1f},{:6.1f},{:6.1f},{:6.1f}'.format(*r['ref_ink']):27s}"
              f" {r['dl']:+6.2f} {r['dt']:+6.2f} {r['dr']:+6.2f} {r['db']:+6.2f} {r['dw']:+6.2f} {r['dh']:+6.2f}"
              f" {'' if r['baseline'] is None else format(r['baseline'], '+.1f'):>5s} {r['mass_ratio'] or 0:.2f}")

    groups = {}
    for r in rows:
        groups.setdefault((r["family"], r["size"], r["bold"]), []).append(r)
    summary = []
    print(f"\n{'family size bold':28s} {'n':>3s} {'med dt':>7s} {'med db':>7s} {'med dh':>7s} {'h ratio':>8s} {'w ratio':>8s} {'baseline':>8s} {'mass':>6s}")
    for (fam, size, bold), rs_ in sorted(groups.items(), key=lambda kv: -len(kv[1])):
        def med(k):
            v = [r[k] for r in rs_ if r[k] is not None]
            return float(np.median(v)) if v else None
        s = dict(family=fam, size=size, bold=bold, n=len(rs_), dt=med("dt"), db=med("db"), dh=med("dh"),
                 h_ratio=med("h_ratio"), w_ratio=med("w_ratio"), baseline=med("baseline"), mass=med("mass_ratio"))
        summary.append(s)
        fmt = lambda v, f="+.2f": "-" if v is None else format(v, f)  # noqa: E731
        print(f"{fam[:16] + ' ' + format(size, 'g') + (' bold' if bold else ''):28s} {len(rs_):3d} {fmt(s['dt']):>7s} {fmt(s['db']):>7s}"
              f" {fmt(s['dh']):>7s} {fmt(s['h_ratio'], '.3f'):>8s} {fmt(s['w_ratio'], '.3f'):>8s} {fmt(s['baseline']):>8s} {fmt(s['mass'], '.2f'):>6s}")
    allb = [r["baseline"] for r in rows if r["baseline"] is not None]
    print(f"\n{len(rows)} labels; baseline |d| <= 0.5 pt: {sum(abs(v) <= 0.5 for v in allb)}/{len(allb)}, "
          f"median {float(np.median(allb)) if allb else 0:+.2f} pt; median dh {float(np.median([r['dh'] for r in rows])) if rows else 0:+.2f} pt")

    out = args.out or os.path.splitext(args.ours)[0] + ".text.json"
    with open(out, "w") as f:
        json.dump({"format": FORMAT, "ref": os.path.abspath(args.ref), "ours": os.path.abspath(args.ours),
                   "crop": args.crop, "units": "pt; d = ours - ref", "rows": rows, "summary": summary}, f, indent=1)
        f.write("\n")
    if args.crops and crops:
        w = max(c.shape[1] for c in crops)
        sheet = np.vstack([np.pad(c, ((0, 2), (0, w - c.shape[1]), (0, 0))) for c in crops])
        cv2.imwrite(args.crops + ".png", sheet)
    print(out, file=sys.stderr)


if __name__ == "__main__":
    main()
