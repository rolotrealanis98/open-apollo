# Plug-in spec runtime, schema 2

This document extends the schema-1 runtime contract (the plugin-spec pipeline's `PIPELINE.md`, including its
"Runtime additions") with what release specs need under the bring-your-own-install model:

- No UA art or prose ships. Sprite fields name **recipe ids**, and the importer (`oa-assets`) regenerates the
  art from the user's own UA install.
- Where UA's editors draw something in code (shell toolbars, captions, LEDs, small displays), plugin-ui draws it
  too, from facts: positions, colours, font names and sizes.

Everything below is implemented in `plugin-ui` (`spec.rs`, `library.rs`, `recipes.rs`, `panel.rs`,
`widgets.rs`). It is tested on synthetic data in `tests/v2.rs` and in the unit tests of `recipes` and `widgets`.

## 1. Spec format changes

| Change | Where | Meaning |
| --- | --- | --- |
| `"schema": 2` | top level | `1` (stage) and `2` (release) are read. Above 2, a spec loads with a warning and unknown keys are ignored. |
| Sprite values are recipe ids | `background`, `elements[].sprite`, `graph.underlay`, `text.spriteFont.sprite`/`decimalSprite`, `toolbar.items[].sprite` | The image is `<art root>/Images/<id>@2x.png` (else `<id>.png`). |
| `text.stringRef` | `elements[].text` | A texts-recipe id (`dll-string`). The imported text is shown instead of `string`, or `""` until it is imported. `text.string` is limited to short labels (48 characters at most, no newline). |
| `parameters[].stepCount`, `parameters[].stepsRef` | `parameters[]` | Withheld step labels (`steps: null`). Labels resolve in this order: the spec's labels; the imported `stepsRef` list, padded from the engine's labels; the engine's labels; then `"1"…"N"`. `steps()` is never below `stepCount`. |
| `text.font` | `text` styles | The font family, a fact such as `"Arial"`. The default is Helvetica, or Menlo for monospaced text. The renderer substitutes a metric-compatible face (Helvetica → Nimbus Sans, Arial → Liberation Sans). |
| `text.italic` | `text` styles | `true` draws the face's italic (or a synthetic slant). |
| `text.valign` | `text` styles | `top`, `center` (default) or `bottom`: where the block of lines sits in the frame (multi-line `stringRef` descriptions start at the top). |
| `text.fit` | `text` styles | What a single line wider than its frame does: `shrink` (the default, down to half size) or `squeeze` (JUCE fitted text: compressed horizontally to 70 %, then truncated with "..."). |
| New element kinds | `elements[].kind` | `toolbar`, `label`, `shape` and `lamp` (§3). A schema-1 runtime ignores them. |
| Removed | `_images`, `provenance`, `notes`, `todo`, `meters`, `meterSources.*.provenance` | Private metadata, never read. |

## 2. Where things are found, and when a panel opens

| What | Search order |
| --- | --- |
| Specs (`SpecLocator`) | `$OA_PLUGIN_SPECS` (alias `$UACONSOLE_PLUGIN_SPECS`), then `$XDG_DATA_HOME/open-apollo/plugin-specs`, then `/usr/share/open-apollo/plugin-specs`. A development stage is used only when one of the variables points at it. |
| Generated art (`ArtLocator`) | `$OA_PLUGIN_ART`, else `$XDG_DATA_HOME/open-apollo/generated`. That folder holds `Images/<id>@2x.png`, `Text/<id>.txt\|.json` and `manifest.json`. |
| Lunacomponents | `$OA_LUNA_COMPONENTS` (alias `$UACONSOLE_LUNA_COMPONENTS`), then `$XDG_DATA_HOME/open-apollo/lunacomponents`, then the macOS install. |

**Folder kinds.**

- A **legacy** folder has `Images/` and no `recipes/`. This is ConsoleLink's stage: sprites sit next to the
  specs, and ids it lacks are also looked up in the art root under the `oa-art:` image scheme.
- Any other folder is a **release** folder. The `spec:` image scheme is rooted at the art root. If the folder
  has `recipes/*.json`, they are loaded; duplicate ids are an error.

`text(id)` reads `Text/<id>.txt` or `.json`. When a recipe pins `output.sha256`, the bytes must match it (for
a `.json` list, the compact UTF-8 JSON). A mismatch reads as not imported.

**`availability(spec)`** returns `Ready`, `Pending(ids)` or `Unavailable(ids)`:

1. **The importer's `manifest.json` is present** (release folders):
   - `specs.<stem>.ready` must be true. That means every sprite the staged spec uses is built or drawn at
     runtime, and none is deferred.
   - Every sprite id the *loaded* spec still names must also be built (`sprites.<id>` is listed and the file
     exists). A runtime id that no patch replaced would leave a hole, so it makes the spec `Unavailable`.
   - Deferred specs (`deferred > 0`) are `Unavailable`. These are the code-drawn tier-4 panels.
   - Specs the importer has not processed yet are `Pending`.
2. **Recipes only:** an id without a buildable recipe (`verified`, `unverified` or `fallback`) is
   `Unavailable`. Recipes with status `needs-tracer`, `no-recipe`, `deferred` or `runtime`, or no recipe at all,
   count as unbuildable. A buildable id that has not been generated is `Pending`.
3. **Legacy folders:** missing files are `Pending`.

`PluginWindow::open` uses the spec panel only when the result is `Ready`. A legacy folder, or
`OA_PLUGIN_WIREFRAME=1` (development), opens it regardless. Otherwise the window shows the generic parameter
list. `PluginWindow::availability` keeps the result, so the host can log it
or run `oa-assets plugins --spec <stem>` and reopen the window.

## 3. Element kinds drawn by our code

All coordinates are @1x points in the element's frame. `#RRGGBB[AA]` colours and every number are facts
measured on the user's plug-in (live editor references, DLL layouts). Code-drawn images are rasterised with
tiny-skia into `runtime:` image keys, cached by a fingerprint of their inputs, and drawn at the panel's scale.

### Per-frame palettes (`label`, `shape`)

`colors` (text or stroke colour) and `fills` (`Fill` values) are arrays indexed by the element's frame. The
frame comes from `frameSource` (any schema-1 type), else from `param` as a switch shows it, else it is 0. The
frame count is `frames`, else the length of the longer array. This is how the on/off look of a caption or a
selector cell follows a parameter, a state or a meter.

A `Fill` is a colour string or a gradient. A gradient is either
`{"linear": [x0, y0, x1, y1], "stops": [[t, colour], …]}` or `{"radial": [cx, cy, r], "stops": […]}`.

### `label`: static text from facts

```jsonc
{"id": "hf-caption", "kind": "label", "x": 344, "y": 318, "w": 34, "h": 15, "param": 25,
 "text": {"string": "HF", "font": "Arial", "size": 11, "weight": "bold", "align": "center", "fit": "squeeze"},
 "colors": ["#8A8A8A", "#FFFFFF"], "fills": ["#00000000", "#1A2A3A"]}
```

- The text is `text.stringRef` (imported) or `text.string`. With neither, a `param`, `meter` or `value` readout
  is used, as for `text`.
- `fills[f]` is drawn behind the text over the whole frame. Text is centred vertically, aligned horizontally,
  shrunk or squeezed per `fit`, and wrapped when `lines` is not 1.
- Labels take no touches. Pair one with a `switch`, `button` or `zone` for behaviour.

### `shape`: vector primitives

```jsonc
{"id": "nut", "kind": "shape", "x": 130, "y": 40, "w": 20, "h": 20,
 "shape": {"type": "hexagon", "fill": {"radial": [8, 8, 12], "stops": [[0, "#D0D0D0"], [1, "#707070"]]},
           "stroke": "#202020", "width": 1},
 "rotate": {"param": 3, "scale": 300}}
```

| `shape.type` | Geometry |
| --- | --- |
| `rect` | The frame, with corner `radius`. |
| `ellipse` | Inscribed in the frame. |
| `line`, `polyline`, `polygon` | `points` (element-local). Polygons close. |
| `arc` | A stroke on the inscribed circle from `from` to `to` degrees clockwise from up. Both are GraphValues, so an arc can follow a parameter or meter. Nothing is drawn when `to ≤ from`. |
| `gear` | `teeth` flat-topped teeth (default 8) with root radius `inner` × outer (default 0.72). |
| `hexagon` | Inscribed, flat sides left and right. |

- `inset` shrinks the frame before drawing.
- `fill` applies only to closed shapes. `stroke` and `width` give the outline; `dash` takes
  `[on, off, …]` points. A stroke is never thinner than one device pixel.
- `fills[f]` and `colors[f]` override `fill` and `stroke` per frame.
- `rotate` and `spin` turn the rasterised shape about its centre, as for sprites.

### `lamp`: LEDs and lamps

```jsonc
{"id": "line-led", "kind": "lamp", "x": 210, "y": 64, "w": 8, "h": 8, "param": 7,
 "lamp": {"on": "#C8FF2A", "off": "#2A3A10", "glow": 4, "highlight": true, "bezel": "#101010", "bezelWidth": 0.5}}
```

- The level is `frame / (frames − 1)`, with `frames` defaulting to 2. The frame comes from `frameSource`
  (`meter` with `threshold`, `blink`, tables) or from `param`. Several frames give intermediate brightness.
- The body is `off` blended toward `on` by the level, with a brighter core. `shape` is `circle` (default) or
  `rect` with corner `radius`.
- `glow` is a radial halo extending that many points beyond the frame, drawn only while lit. `highlight` adds a
  specular spot, and `bezel` adds an outline.

### `toolbar`: plug-in shell strips

Brainworx / Plugin Alliance and Softube editors draw their top toolbar in shell code, around a few bitmaps
that every one of their DLLs carries. A `toolbar` element draws the strip and its controls:

```jsonc
{"id": "chrome-toolbar", "kind": "toolbar", "x": 0, "y": 0, "w": 615, "h": 24,
 "toolbar": {"family": "brainworx", "track": "#B4B4B4", "accent": "#0D759F", "items": [
   {"type": "switch", "x": 0,     "w": 24, "h": 24, "param": 0, "sprite": "chrome.bx.power", "frames": 2, "glyph": "power"},
   {"type": "icon",   "x": 36.5,  "w": 24, "h": 24, "sprite": "chrome.bx.undo", "frames": 2, "glyph": "undo"},
   {"type": "bank",   "x": 93.5,  "w": 96, "h": 24, "param": 17, "sprite": "chrome.bx.bank", "frames": 2, "cells": 4, "glyph": "ABCD"},
   {"type": "icon",   "x": 366,   "w": 68.5, "h": 24, "sprite": "ada_flanger.tb.saturationlabel", "frames": 1},
   {"type": "knob",   "x": 435,   "w": 18.5, "h": 24, "param": 13, "ring": {"cx": 9.2, "cy": 12.4, "r": 6.6, "width": 2.4, "gap": 70}},
   {"type": "gear",   "x": 1048,  "y": 6, "w": 11, "h": 11, "color": "#E5E5E5"},
   {"type": "rect",   "x": 1070,  "w": 90, "h": 24, "color": "#019C74"}]}}
```

- **`family`** sets the defaults:

  | Family | `color` (bar) | `track` | `accent` | Glyph ink |
  | --- | --- | --- | --- | --- |
  | `brainworx` | `#000000` | `#B4B4B4` | `#0D759F` | `#D2D2D2` |
  | `softube` | `#444444` | | | `#F0F0F0` |

  The `color`, `track` and `accent` keys override them.
- **Items** are drawn in order and clipped to the toolbar. Their `x`, `y`, `w` and `h` are toolbar-local
  points.

| `type` | Draws | Takes |
| --- | --- | --- |
| `icon` | Frame `frame` (default 0) of the `frames`-tall sprite, stretched to the item. | nothing |
| `switch` | The `param`'s step as the frame. For 2 frames, frame 1 is "on" (step > 0); `invert` swaps them, as for a bypass parameter whose lit look is step 0. | A tap writes the next step. Two-state parameters without labels count as 2 steps. Rack route: `PluginStep(param, next)`. |
| `bank` | `cells` (default: the parameter's steps, else 4) horizontal cells cut from the sprite. Cell `c` shows frame 1 when it is the selected step, else frame 0. `pitch` spaces the cells wider than the art. | A tap on a cell selects that step. Rack route: `PluginStep(param, c)`. |
| `knob` | An open ring: `track` over the sweep, then the value arc in `accent`, clockwise from the gap at the bottom (`gap` degrees, default 70; butt ends). | A vertical drag, as for spec knobs. A double tap resets. Rack route: `PluginParam` drag. |
| `gear` | A gear glyph in `color`, our own drawing. | nothing |
| `rect` | A filled rectangle in `color`, e.g. a logo button's background. | nothing |

- **Fallbacks while the art is missing.** An `icon` or `switch` whose sprite is not generated draws its
  `glyph`: `power`, `undo`, `redo` and `gear` are vector glyphs, and any other string is drawn as bold text.
  A `bank` without art draws `glyph`'s letters (default A, B, C, …) in `accent` (selected) or ink. The
  availability rules (§2) still keep such a panel closed in release folders; the fallbacks serve development
  and partial installs.
- **Bound parameters.** Toolbar `switch`, `bank` and `knob` items bind their parameters, so those parameters
  leave the "more" panel.

**Shared art ids.** The Brainworx shell icons are carved 2-frame @2x PNGs, pixel-identical in 54 Powered DLLs:
`chrome.bx.power`, `.undo`, `.redo`, `.bank` (A–D, 4 × 48 px cells), `.bank-compact`, `.copy`, `.paste`,
`.reset`, `.midi`, `.lfo`, `.note` and `.digits`. Toolbar bitmaps found in two or more DLLs are
`chrome.bx.<resource>`, for example `chrome.bx.toolbaraboutbnt` and `chrome.bx.mix`. A plug-in's own
bitmaps are `<spec>.tb.<resource>`. Softube's are `chrome.softube.toggle` (MPACK 507),
`chrome.softube.logo` and `chrome.softube.smiley` (crops of MPACK 509). Softube's art is 1x, scaled 2x with
LANCZOS.

## 4. Spec patches (overlay format v1)

Patches turn staged specs, whose toolbars, captions and small displays were cut from live captures, into
specs that use the elements above. A patch is one JSON object per spec, `<stem>.json`:

| Key | Meaning |
| --- | --- |
| `patchSchema` | `1` |
| `spec` | The spec stem. |
| `base` | `{"sha256": …}` of the spec file the patch was made against. A mismatch only warns. |
| `set` | Top-level fields to replace; `null` deletes (e.g. `{"background": null}`). |
| `removeElements` | Element ids to remove. |
| `elementPatches` | `{id: {field: value \| null}}`, a shallow merge; `null` deletes a field. |
| `parameterPatches` | `{"<index>": {field: value \| null}}` |
| `addElements` | Elements to insert, each with at most one placement key, removed on insert: `"before": id`, `"after": id` or `"at": index`. The default is to append. |
| `resolves`, `needs`, `notes`, `owners` | Bookkeeping, not applied. `resolves` maps each replaced staged sprite id to how it is drawn now; `needs` lists the new recipe ids. |

Order of application: `set`, `removeElements`, `elementPatches`, `parameterPatches`, then `addElements` in
list order. An added id that already exists is an error.

Several owners may write parts (`parts/<owner>/<stem>.json`). The merge concatenates lists and merges maps.
The same element id touched by two owners, or the same `set` or `parameterPatches` key, is an error.

## 5. Limits

- Text is drawn by the renderer's text engine. It cannot be rotated, and fonts are matched by family name
  (§1).
- Lamps have no blur beyond the radial glow, and there is no inner shadow or bevel. Captured looks that need
  these stay DLL art.
- Toolbar fallbacks are approximations. The true look comes from the recipe sprites.
