#!/usr/bin/env -S python3 -I
"""Locate UA skin images in a reference Console shot and in our render, pair them, report offsets.

  tools/align/placement_diff.py REF.png OURS.png [--crop x,y,w,h] [--skins SPEC] [--draws auto|none|FILE]
                                [--out diff.json] [--threshold 0.85] [--max-dist 16] [--tol 0.25]
                                [--moved] [--overlay PREFIX]

Both images are searched with ConsoleLab's template matcher (tools/template_match.py: masked
TM_CCOEFF_NORMED, coarse-to-fine; filmstrip frames recovered in place; on/off states compete;
9-slice meter ends paired). Each detection in REF is paired with the nearest compatible one in
OURS (same image, same filmstrip, or another state of the same skin) within --max-dist pt.

Which images are searched:
  * --draws (default `auto`: OURS with `.draws.json`, written by `uaconsole --render`): every UA
    image our renderer drew inside the crop, at its drawn size (9-slices rendered at that size, plug-in
    icons scaled), plus the other state images of every skin that uses it (catches on/off and
    alternative-art differences). OURS detections also carry the drawn rect (`drawn`).
  * --skins SPEC (adds to the drawn set; the only source when there is no sidecar): comma list of
    areas (strip, monitor, ...), skin keys (`views/Switch-Small` or `Switch-Small`), and globs over
    keys or image paths (`views/Switch*`, `img:buttons/btn_category_*`).

Output (stdout): a table, one row per pair, sorted top-to-bottom:
  name  state(ref>ours)  ref x,y,w,h  ours x,y,w,h  dx dy dw dh  score ref/ours
  d = OURS - REF in points (dx > 0: ours is further right). Then unmatched-in-ref (art the
  reference shows that we do not draw there) and unmatched-in-ours.
JSON (--out, default OURS.placement.json): format "placement-diff/1", the same rows with pixel and
point rects, plus `summary`. The interface (arguments, JSON keys) is stable; new keys may be added.

Coordinates are window points; --crop is in points and applies to both images (--ours-crop if
OURS is framed differently). Scales come from REF's refshot sidecar (REF.json "scale"), OURS's
draws sidecar ("scale"), else 2; override with --ref-scale / --ours-scale.

REF, OURS and --overlay images contain UA art: keep them out of git.
Environment: CONSOLELAB_TOOLS (default ~/github/ConsoleLab/tools), plus template_match's
CONSOLELAB_RESOURCES / CONSOLELAB_ORACLE / CONSOLELAB_VENV.
"""
import os
import sys

TOOLS = os.environ.get("CONSOLELAB_TOOLS") or os.path.expanduser("~/github/ConsoleLab/tools")
sys.path.insert(0, TOOLS)
try:
    import skinart  # noqa: E402
except ImportError:
    sys.exit(f"placement_diff: ConsoleLab tools not found at {TOOLS} (set CONSOLELAB_TOOLS)")

skinart.bootstrap(__file__)  # re-exec this script in the venv (python -I)

import argparse  # noqa: E402
import datetime  # noqa: E402
import fnmatch  # noqa: E402
import json  # noqa: E402
import math  # noqa: E402
import time  # noqa: E402
from concurrent.futures import ThreadPoolExecutor  # noqa: E402

import cv2  # noqa: E402
import numpy as np  # noqa: E402

import template_match as tm  # noqa: E402

FORMAT = "placement-diff/1"

# Extra areas for --skins (in addition to skinart.AREAS: strip, monitor). Globs over skin keys.
AREAS = {
    "strip-full": ["faders/Main-Input-Fader", "meters/*Input-Meter", "knobs/Pan-*", "views/Switch-Small*",
                   "backgrounds/Channel-Bottom", "backgrounds/Fader-Ref-Mark-Dark", "inserts/*"],
    "buttons": ["views/*"],
    "knobs": ["knobs/*"],
}


# ---------------------------------------------------------------------------------------------
# Images

def plugin_icon_path(name):
    for base in (os.path.join(skinart.RESOURCES, "Plug-In Icons"),
                 "/Library/Application Support/Universal Audio/Plug-In Icons"):
        p = os.path.join(base, name + ".png")
        if os.path.exists(p):
            return p
    return None


_ART = {}


def load_key(key):
    """BGRA art for a draw key (`images/`-relative path or `plugin:<EffectName>`), or None."""
    if key in _ART:
        return _ART[key]
    im = None
    if key.startswith("plugin:"):
        p = plugin_icon_path(key[len("plugin:"):])
        if p:
            im = cv2.imread(p, cv2.IMREAD_UNCHANGED)
            if im is not None:
                if im.ndim == 2:
                    im = cv2.cvtColor(im, cv2.COLOR_GRAY2BGRA)
                elif im.shape[2] == 3:
                    im = np.dstack([im, np.full(im.shape[:2], 255, np.uint8)])
    else:
        try:
            im = skinart.load_bgra(key)
        except (FileNotFoundError, cv2.error):
            im = None
    _ART[key] = im
    return im


class DrawTemplate(skinart.Template):
    """A template whose art is prepared from a draw: a 9-slice rendered at its drawn size, or a
    plain image whose density follows its drawn size (art px per pt)."""

    def __init__(self, key, group, image, part, density, art, skins=(), ring_pt=None):
        super().__init__(key, group, "image", image, part, density, skins=list(skins), ring_pt=ring_pt)
        self._art = art

    def art(self):
        return self._art


class FilmTemplate(skinart.Template):
    """A filmstrip drawn at some size: frame templates keyed by that size (not just the image).
    `drawn_frames`: frames OURS drew (searched in addition to the representative frames)."""
    drawn_frames = ()

    def frame_tpl(self, i):
        t = skinart.Template(f"{self.key}#{i}", self.group, "film", self.image, self.part, self.density,
                             skins=self.skins, frame=i, film=self.film)
        return t


# ---------------------------------------------------------------------------------------------
# Skin index: image path -> skins and their state images

class SkinIndex:
    def __init__(self, skins):
        self.skins = skins
        self.by_image = {}
        for key, s in skins.skins.items():
            for part, val in (s.get("img") or {}).items():
                if val:
                    self.by_image.setdefault(val[0], []).append((key, part))

    def skins_of(self, image):
        return sorted({k for k, _ in self.by_image.get(image, [])})

    def siblings(self, image):
        """(image, part, skin) of every state image of the skins that use `image` (same size)."""
        out = []
        for key in self.skins_of(image):
            s = self.skins.skins[key]
            if "film" in s or "fader" in s:
                continue
            for part, val in (s.get("img") or {}).items():
                if not val or val[0] == image or any(p in part for p in skinart.SKIP_PARTS):
                    continue
                out.append((val[0], part, key))
        return out

    def expand(self, spec):
        """Skin keys for a --skins spec (areas, keys, globs)."""
        keys = []

        def add(k):
            if k not in keys:
                keys.append(k)
        for item in (s.strip() for s in spec.split(",")):
            if not item or item.startswith("img:"):
                continue
            if item in skinart.AREAS:
                for k in skinart.AREAS[item]:
                    add(k)
            elif item in AREAS:
                for pat in AREAS[item]:
                    for k in sorted(fnmatch.filter(self.skins.skins, pat)):
                        add(k)
            elif any(c in item for c in "*?["):
                pats = [item] if "/" in item else [f"*/{item}"]
                for p in pats:
                    for k in sorted(fnmatch.filter(self.skins.skins, p)):
                        add(k)
            else:
                add(self.skins.resolve(item))
        return keys

    def image_globs(self, spec):
        pats = [s.strip()[4:] for s in spec.split(",") if s.strip().startswith("img:")]
        return sorted({img for img in self.by_image for p in pats if fnmatch.fnmatch(img, p)})


# ---------------------------------------------------------------------------------------------
# Templates

def intersect(a, b):
    """Intersection of two x,y,w,h rects (None = unbounded); None when empty."""
    if a is None or b is None:
        return a if b is None else b
    x0, y0 = max(a[0], b[0]), max(a[1], b[1])
    x1, y1 = min(a[0] + a[2], b[0] + b[2]), min(a[1] + a[3], b[1] + b[3])
    return [x0, y0, x1 - x0, y1 - y0] if x1 > x0 and y1 > y0 else None


def family_groups(templates):
    """Union templates whose skins overlap or that share an image: one NMS group per family, so
    the states of one control compete at a place and different controls never suppress each other."""
    parent = {}

    def find(a):
        while parent.setdefault(a, a) != a:
            parent[a] = parent[parent[a]]
            a = parent[a]
        return a

    def union(a, b):
        parent[find(a)] = find(b)
    for t in templates:
        n = "img:" + t.image
        for s in t.skins:
            union(n, "skin:" + s)
    for t in templates:
        t.group = "fam:" + find("img:" + t.image)
    return templates


def draw_templates(draws, crop, index, min_pt, log, ours=None, ours_scale=2.0):
    """Templates for the images drawn inside `crop` (pt), deduplicated by image and drawn size.
    With OURS pixels, art that OURS shows covered (labels, children, pointers) is masked out."""
    out = {}
    drawn = []
    inst = {}
    for d in draws.get("images", []):
        x, y, w, h = d["dst"]
        vis = intersect(intersect(d["dst"], d.get("clip")), crop)
        if not vis or vis[2] < min_pt or vis[3] < min_pt or w < min_pt or h < min_pt:
            continue
        key = d["key"]
        im = load_key(key)
        if im is None:
            log(f"skip {key}: not loadable")
            continue
        drawn.append(d)
        skins = index.skins_of(key)
        part = (index.by_image.get(key) or [(None, "image")])[0][1]
        fr = d.get("frame")
        if fr:
            idx, fh = fr
            n = im.shape[0] // fh
            dens = im.shape[1] / w
            tk = f"{key}@film{fh}:{w:g}x{h:g}"
            if tk not in out:
                out[tk] = FilmTemplate(tk, "film", "film", key, part, dens, skins=list(skins),
                                       film={"fh": fh, "n": n})
                out[tk].drawn_frames = set()
            out[tk].drawn_frames.add(idx)
            continue
        sl = d.get("slice")
        ring = 4 if sl and min(w, h) >= 40 else None  # panels host children: compare their border
        if sl:
            W, H = max(1, int(round(w * 2))), max(1, int(round(h * 2)))
            tk = f"{key}@9s:{w:g}x{h:g}"
            inst.setdefault(tk, []).append(d)
            if tk not in out:
                art = skinart.nine_slice(im, [int(v) for v in sl], W, H)
                out[tk] = DrawTemplate(tk, "", key, part, 2.0, art, skins, ring)
            continue
        dens = im.shape[1] / w
        native = abs(dens - 2.0) < 0.02 and abs(im.shape[0] / h - 2.0) < 0.02
        tk = key if native else f"{key}@{w:g}x{h:g}"
        inst.setdefault(tk, []).append(d)
        if tk not in out:
            art = im if native else skinart.resize(im, max(1, int(round(w * 2))), max(1, int(round(h * 2))))
            out[tk] = DrawTemplate(tk, "", key, part, 2.0, art, skins, ring)
        # The other states of the same skins, at the same drawn size.
        for sib, spart, skin in index.siblings(key):
            sim = load_key(sib)
            if sim is None or sim.shape[:2] != im.shape[:2]:
                continue
            sk = sib if native else f"{sib}@{w:g}x{h:g}"
            if sk in out:
                if skin not in out[sk].skins:
                    out[sk].skins.append(skin)
                continue
            art = sim if native else skinart.resize(sim, max(1, int(round(w * 2))), max(1, int(round(h * 2))))
            out[sk] = DrawTemplate(sk, "", sib, spart, 2.0, art, [skin], ring)
    if ours is not None:
        for tk, ds in inst.items():
            occlusion_mask(out[tk], ds, ours, ours_scale)
    return list(out.values()), drawn


def occlusion_mask(t, instances, ours, scale, tol=40, vote=0.5):
    """Zero the alpha of art pixels that OURS shows covered at most of the places the art was
    drawn (labels on buttons, children on panels, a pointer over a knob). Template and covering
    content are then matched on what both renderers actually show. Tinted draws are skipped."""
    art = t._art
    ah, aw = art.shape[:2]
    opaque = art[:, :, 3] >= 250
    if not opaque.any():
        return
    votes = np.zeros((ah, aw), np.int32)
    n = 0
    for d in instances:
        if d.get("tint") and any(abs(v - 1) > 1e-3 for v in d["tint"]):
            continue
        x, y, w, h = d["dst"]
        clip = d.get("clip")
        if clip and intersect(d["dst"], clip) != [x, y, w, h]:
            continue
        x0, y0 = int(round(x * scale)), int(round(y * scale))
        pw, ph = int(round(w * scale)), int(round(h * scale))
        if x0 < 0 or y0 < 0 or x0 + pw > ours.shape[1] or y0 + ph > ours.shape[0] or pw < 1 or ph < 1:
            continue
        patch = ours[y0:y0 + ph, x0:x0 + pw]
        if (pw, ph) != (aw, ah):
            patch = cv2.resize(patch, (aw, ah), interpolation=cv2.INTER_AREA)
        diff = np.abs(patch.astype(np.int16) - art[:, :, :3].astype(np.int16)).max(axis=2)
        votes += (diff <= tol)
        n += 1
    if not n:
        return
    keep = opaque & (votes >= vote * n)
    if keep.sum() < 0.2 * opaque.sum() or keep.sum() < 16:
        return  # mostly covered: keep the plain art (and its ring, if any)
    if keep.sum() < opaque.sum():
        a = art.copy()
        a[:, :, 3] = np.where(opaque & ~keep, 0, a[:, :, 3])
        t._art = a
        t.ring_pt = None
        t.occluded = round(1 - keep.sum() / opaque.sum(), 3)


def skin_templates(index, spec, log):
    keys = index.expand(spec)
    temps = skinart.templates_for(index.skins, keys, warn=log) if keys else []
    for img in index.image_globs(spec):
        im = load_key(img)
        if im is not None:
            temps.append(DrawTemplate(img, "", img, "image", 2.0, im, index.skins_of(img)))
    return temps, keys


# ---------------------------------------------------------------------------------------------
# Matching (template_match.main's pipeline, on one image)

def match_image(img, templates, args, scale):
    a = argparse.Namespace(**vars(args))
    a.ref_scale = scale
    m = tm.Matcher(img, a)
    # Film representatives are searched with slack: a knob whose pointer sits between two
    # representatives correlates below the threshold until best_frame tries every frame in place.
    af = argparse.Namespace(**vars(a))
    af.threshold = max(0.5, args.threshold - args.film_slack)
    mf = tm.Matcher(img, af)
    mf.gray, mf.cache = m.gray, m.cache
    films = [t for t in templates if t.kind == "film"]
    with ThreadPoolExecutor(max_workers=args.jobs) as ex:
        reps = list(ex.map(lambda t: m.film_reps(t, 1.0), films))
    reps = [sorted(set(rs) | set(t.drawn_frames or ())) for t, rs in zip(films, reps)]
    jobs = [(m, t) for t in templates if t.kind != "film"]
    jobs += [(mf, t.frame_tpl(i)) for t, rs in zip(films, reps) for i in rs]
    with ThreadPoolExecutor(max_workers=args.jobs) as ex:
        found = [d for ds in ex.map(lambda mt: mt[0].search(mt[1], 1.0), jobs) for d in ds]
    found = [d for d in found if d["rmse"] <= args.max_rmse]
    dets = tm.nms(found, args.nms_iou, key="image_key")
    film_dets = [d for d in dets if d["tpl"].kind == "film"]
    with ThreadPoolExecutor(max_workers=args.jobs) as ex:
        list(ex.map(mf.best_frame, film_dets))
    dets = [d for d in dets if d["score"] >= args.threshold]
    dets = tm.nms(dets, args.nms_iou, matcher=m)
    tm.pick_states(m, dets, templates)
    dets, _loose = tm.pair_ends(dets, int(10 * scale))
    return cross_family(dets, args.cross_margin)


def section_of(d):
    t = d["tpl"]
    return (t.skins[0].split("/", 1)[0] if t.skins else t.image.split("/", 1)[0])


def cross_family(dets, margin):
    """Different families of one skin section matched at one place (a 4-pt L meter inside an 8-pt
    mono meter): keep the clearly better one. Ties (within `margin`) both stay."""
    order = sorted(dets, key=lambda d: -tm.quality(d))
    keep = []
    for d in order:
        sec = section_of(d)
        if any(section_of(k) == sec and k["tpl"].group != d["tpl"].group and contained(d, k) > 0.8
               and tm.quality(k) >= tm.quality(d) + margin for k in keep):
            continue
        keep.append(d)
    return keep


def contained(a, b):
    """Fraction of a's area inside b."""
    ix = max(0, min(a["x"] + a["w"], b["x"] + b["w"]) - max(a["x"], b["x"]))
    iy = max(0, min(a["y"] + a["h"], b["y"] + b["h"]) - max(a["y"], b["y"]))
    return ix * iy / float(max(1, a["w"] * a["h"]))


def record(d, ox, oy, scale, index):
    t = d["tpl"]
    x, y = d["x"] + ox, d["y"] + oy
    skins = list(t.skins) or index.skins_of(t.image)
    return {
        "name": skins[0] if len(skins) == 1 else t.image,
        "skins": skins,
        "image": t.image,
        "part": t.part,
        "frame": d.get("frame") if t.kind == "film" else None,
        "frames": t.film["n"] if t.film else None,
        "group": t.group,
        "rect": [round(x / scale, 3), round(y / scale, 3), round(d["w"] / scale, 3), round(d["h"] / scale, 3)],
        "rect_px": [int(x), int(y), int(d["w"]), int(d["h"])],
        "score": round(float(d["score"]), 4),
        "rmse": round(float(d["rmse"]), 2),
    }


def state(r):
    if r["frame"] is not None:
        return f"f{r['frame']}"
    return r["part"] or ""


# ---------------------------------------------------------------------------------------------
# Pairing

def compatible(a, b):
    return a["image"] == b["image"] or a["group"] == b["group"] or bool(set(a["skins"]) & set(b["skins"]))


def centre(r):
    x, y, w, h = r["rect"]
    return x + w / 2, y + h / 2


def pair(refs, ours, max_dist):
    cands = []
    for i, a in enumerate(refs):
        ca = centre(a)
        for j, b in enumerate(ours):
            if not compatible(a, b):
                continue
            cb = centre(b)
            dist = math.hypot(cb[0] - ca[0], cb[1] - ca[1])
            if dist > max_dist:
                continue
            size = abs(a["rect"][2] - b["rect"][2]) + abs(a["rect"][3] - b["rect"][3])
            cost = dist + 0.25 * size + (0 if a["image"] == b["image"] else 0.5)
            cands.append((cost, i, j))
    cands.sort()
    used_r, used_o, pairs = set(), set(), []
    for _, i, j in cands:
        if i in used_r or j in used_o:
            continue
        used_r.add(i)
        used_o.add(j)
        pairs.append((refs[i], ours[j]))
    return pairs, [r for i, r in enumerate(refs) if i not in used_r], [o for j, o in enumerate(ours) if j not in used_o]


def attach_drawn(ours, drawn):
    """OURS detection -> the drawn dst of the same image nearest to it (pt)."""
    for o in ours:
        best = None
        for d in drawn:
            if d["key"] != o["image"]:
                continue
            x, y, w, h = d["dst"]
            dist = math.hypot(x + w / 2 - centre(o)[0], y + h / 2 - centre(o)[1])
            if best is None or dist < best[0]:
                best = (dist, d)
        if best and best[0] <= 4:
            o["drawn"] = best[1]["dst"]


# ---------------------------------------------------------------------------------------------

def parse_rect(s):
    v = [float(x) for x in s.split(",")]
    if len(v) != 4:
        raise argparse.ArgumentTypeError("expected x,y,w,h")
    return v


def crop_img(img, crop, scale):
    if not crop:
        return img, 0, 0
    x, y, w, h = (int(round(v * scale)) for v in crop)
    x0, y0 = max(0, x), max(0, y)
    return np.ascontiguousarray(img[y0:y + h, x0:x + w]), x0, y0


def fmt_rect(r):
    return "{:7.1f},{:6.1f} {:5.1f}x{:<5.1f}".format(*r)


def draw_overlay(path, img, pairs_side, unmatched, scale):
    out = img.copy()
    t = max(1, int(scale // 2))
    for r in pairs_side:
        x, y, w, h = r["rect_px"]
        cv2.rectangle(out, (x, y), (x + w - 1, y + h - 1), (80, 220, 80), t)
    for r in unmatched:
        x, y, w, h = r["rect_px"]
        cv2.rectangle(out, (x, y), (x + w - 1, y + h - 1), (60, 60, 255), t)
        cv2.putText(out, os.path.basename(r["image"]).split(".")[0][:28], (x, max(10, y - 3)),
                    cv2.FONT_HERSHEY_SIMPLEX, 0.3 * scale, (60, 60, 255), 1, cv2.LINE_AA)
    cv2.imwrite(path, out)


def main():
    ap = argparse.ArgumentParser(description=__doc__.split("\n\n")[0],
                                 formatter_class=argparse.RawDescriptionHelpFormatter, epilog=__doc__.split("\n\n", 1)[1])
    ap.add_argument("ref")
    ap.add_argument("ours")
    ap.add_argument("--crop", type=parse_rect, help="region x,y,w,h in pt (both images)")
    ap.add_argument("--ours-crop", type=parse_rect, help="region in OURS when framed differently (default --crop)")
    ap.add_argument("--skins", default="", help="areas, skin keys, globs, img:<glob> (adds to --draws)")
    ap.add_argument("--draws", default="auto", help="auto (OURS.draws.json if present) | none | FILE")
    ap.add_argument("--out", help="JSON output (default OURS.placement.json)")
    ap.add_argument("--ref-scale", type=float, help="REF px per pt (default: REF.json 'scale', else 2)")
    ap.add_argument("--ours-scale", type=float, help="OURS px per pt (default: draws 'scale', else REF's)")
    ap.add_argument("--threshold", type=float, default=0.85, help="min masked TM_CCOEFF_NORMED (default 0.85)")
    ap.add_argument("--max-rmse", type=float, default=48.0, help="max colour RMS error over opaque pixels")
    ap.add_argument("--max-dist", type=float, default=16.0, help="max centre distance for a pair, pt (default 16)")
    ap.add_argument("--min-size", type=float, default=4.0, help="skip drawn images smaller than this, pt")
    ap.add_argument("--tol", type=float, default=0.25, help="|d| <= tol pt counts as aligned (default 0.25)")
    ap.add_argument("--moved", action="store_true", help="table: only pairs off by more than --tol")
    ap.add_argument("--overlay", help="write PREFIX-ref.png / PREFIX-ours.png with boxes (UA art: keep out of git)")
    ap.add_argument("--no-occlusion", action="store_true",
                    help="do not mask art that OURS shows covered (labels, children) out of the templates")
    ap.add_argument("--jobs", type=int, default=min(8, os.cpu_count() or 1))
    ap.add_argument("--quiet", action="store_true")
    # template_match.Matcher knobs
    ap.add_argument("--nms-iou", type=float, default=0.3)
    ap.add_argument("--variant-eps", type=float, default=0.01)
    ap.add_argument("--coarse-drop", type=float, default=0.25)
    ap.add_argument("--min-coarse-px", type=int, default=12)
    ap.add_argument("--max-candidates", type=int, default=400)
    ap.add_argument("--film-frames", type=int, default=0)
    ap.add_argument("--film-cover", type=float, default=0.8)
    ap.add_argument("--film-slack", type=float, default=0.25,
                    help="filmstrip representatives may score this much below --threshold before frame refinement")
    ap.add_argument("--cross-margin", type=float, default=0.05,
                    help="drop a match inside a better one of another family of the same section by this quality margin")
    args = ap.parse_args()
    log = (lambda *a: None) if args.quiet else (lambda *a: print(*a, file=sys.stderr))
    t0 = time.time()

    side = tm.sidecar(args.ref)
    if args.ref_scale is None:
        args.ref_scale = float(side["scale"]) if side and side.get("scale") else 2.0
    draws = None
    dpath = None
    if args.draws != "none":
        dpath = os.path.splitext(args.ours)[0] + ".draws.json" if args.draws == "auto" else args.draws
        if os.path.exists(dpath):
            with open(dpath) as f:
                draws = json.load(f)
        elif args.draws != "auto":
            sys.exit(f"placement_diff: no draws file {dpath}")
        else:
            dpath = None
    if args.ours_scale is None:
        args.ours_scale = float(draws["scale"]) if draws and draws.get("scale") else args.ref_scale

    ref_full = tm.load_ref(args.ref)
    ours_full = tm.load_ref(args.ours)
    index = SkinIndex(skinart.Skins())
    templates, drawn, keys = [], [], []
    if draws:
        templates, drawn = draw_templates(draws, args.ours_crop or args.crop, index, args.min_size, log,
                                          None if args.no_occlusion else ours_full, args.ours_scale)
    if args.skins:
        st, keys = skin_templates(index, args.skins, log)
        have = {t.key for t in templates}
        templates += [t for t in st if t.key not in have]
    if not templates:
        sys.exit("placement_diff: nothing to search (no OURS.draws.json in the crop and no --skins)")
    family_groups(templates)
    log(f"placement_diff: {len(templates)} templates ({len(drawn)} draws{', ' + str(len(keys)) + ' skins' if keys else ''})")

    ref_img, rox, roy = crop_img(ref_full, args.crop, args.ref_scale)
    ours_img, oox, ooy = crop_img(ours_full, args.ours_crop or args.crop, args.ours_scale)
    dref = match_image(ref_img, templates, args, args.ref_scale)
    dours = match_image(ours_img, templates, args, args.ours_scale)
    refs = [record(d, rox, roy, args.ref_scale, index) for d in dref]
    ours = [record(d, oox, ooy, args.ours_scale, index) for d in dours]
    if args.ours_crop and args.crop:  # report OURS in REF's frame
        sx, sy = args.crop[0] - args.ours_crop[0], args.crop[1] - args.ours_crop[1]
        for o in ours:
            o["rect"] = [round(o["rect"][0] + sx, 3), round(o["rect"][1] + sy, 3), *o["rect"][2:]]
    if drawn:
        attach_drawn(ours, drawn)
    pairs, un_ref, un_ours = pair(refs, ours, args.max_dist)
    seen = {r["image"] for r in refs + ours}
    not_located = sorted({t.image for t in templates if t.image not in seen and t.image in {d["key"] for d in drawn}})
    pairs.sort(key=lambda p: (round(p[0]["rect"][1]), p[0]["rect"][0]))

    rows = []
    for a, b in pairs:
        d = [round(b["rect"][k] - a["rect"][k], 3) for k in range(4)]
        rows.append({
            "name": a["name"], "image_ref": a["image"], "image_ours": b["image"],
            "state_ref": state(a), "state_ours": state(b),
            "ref": a["rect"], "ours": b["rect"], "ref_px": a["rect_px"], "ours_px": b["rect_px"],
            "dx": d[0], "dy": d[1], "dw": d[2], "dh": d[3],
            "score_ref": a["score"], "score_ours": b["score"],
            "aligned": all(abs(v) <= args.tol for v in d),
            **({"drawn": b["drawn"]} if "drawn" in b else {}),
        })

    # Table.
    out = sys.stdout
    shown = [r for r in rows if not (args.moved and r["aligned"])]
    w = max([len(r["name"]) for r in shown] + [20])
    w = min(w, 44)
    print(f"{'skin/image':{w}s} {'state ref>ours':18s} {'ref x,y w x h (pt)':27s} {'ours x,y w x h (pt)':27s}"
          f" {'dx':>6s} {'dy':>6s} {'dw':>6s} {'dh':>6s}  score", file=out)
    for r in shown:
        st = r["state_ref"] if r["state_ref"] == r["state_ours"] else f"{r['state_ref']}>{r['state_ours']}"
        flag = " " if r["aligned"] else "*"
        print(f"{r['name'][-w:]:{w}s} {st[:18]:18s} {fmt_rect(r['ref'])} {fmt_rect(r['ours'])}"
              f" {r['dx']:+6.2f} {r['dy']:+6.2f} {r['dw']:+6.2f} {r['dh']:+6.2f} {flag}{r['score_ref']:.2f}/{r['score_ours']:.2f}",
              file=out)
    for title, lst in (("unmatched in REF (shown by Console, not found in ours)", un_ref),
                       ("unmatched in OURS (we draw it, Console does not show it here)", un_ours)):
        print(f"\n{title}: {len(lst)}", file=out)
        for r in sorted(lst, key=lambda r: (round(r["rect"][1]), r["rect"][0])):
            print(f"  {r['name'][-w:]:{w}s} {state(r)[:18]:18s} {fmt_rect(r['rect'])}  score {r['score']:.2f}"
                  f"  {os.path.basename(r['image'])}", file=out)

    if not_located:
        print(f"\nnot located in either (drawn by us; covered, flat or tinted art): {len(not_located)}", file=out)
        for img in not_located:
            print(f"  {img}", file=out)
    moved = [r for r in rows if not r["aligned"]]
    summary = {
        "pairs": len(rows), "aligned": len(rows) - len(moved), "moved": len(moved),
        "unmatched_ref": len(un_ref), "unmatched_ours": len(un_ours),
        "median_dx": float(np.median([r["dx"] for r in rows])) if rows else None,
        "median_dy": float(np.median([r["dy"] for r in rows])) if rows else None,
        "max_abs_d": max([max(abs(r["dx"]), abs(r["dy"]), abs(r["dw"]), abs(r["dh"])) for r in rows], default=None),
        "seconds": round(time.time() - t0, 1),
    }
    print(f"\n{summary['pairs']} pairs: {summary['aligned']} aligned (<= {args.tol} pt), {summary['moved']} off;"
          f" {summary['unmatched_ref']} only in REF, {summary['unmatched_ours']} only in OURS"
          f" [{summary['seconds']} s]", file=out)

    doc = {
        "format": FORMAT,
        "source": {"ref": os.path.abspath(args.ref), "ours": os.path.abspath(args.ours), "draws": dpath and os.path.abspath(dpath),
                   "crop": args.crop, "ours_crop": args.ours_crop, "ref_scale": args.ref_scale, "ours_scale": args.ours_scale,
                   "skins": keys, "templates": len(templates), "threshold": args.threshold, "max_dist": args.max_dist,
                   "tol": args.tol, "tool": "console-rs/tools/align/placement_diff.py",
                   "generated": datetime.datetime.now(datetime.timezone.utc).isoformat(timespec="seconds")},
        "units": "pt; d = ours - ref",
        "pairs": rows,
        "unmatched_ref": un_ref,
        "unmatched_ours": un_ours,
        "not_located": not_located,
        "summary": summary,
    }
    for lst in (un_ref, un_ours):
        for r in lst:
            r.pop("group", None)
    out_path = args.out or os.path.splitext(args.ours)[0] + ".placement.json"
    with open(out_path, "w") as f:
        json.dump(doc, f, indent=1)
        f.write("\n")
    log(f"placement_diff: wrote {out_path}")

    if args.overlay:
        draw_overlay(args.overlay + "-ref.png", ref_full, [a for a, _ in pairs], un_ref, args.ref_scale)
        ox = ours_full
        draw_overlay(args.overlay + "-ours.png", ox, [b for _, b in pairs], un_ours, args.ours_scale)


if __name__ == "__main__":
    main()
