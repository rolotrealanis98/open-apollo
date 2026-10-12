# UAD Console plug-in window chrome (title bar, toolbar, footer)

Source: `Rack.dll` (UAD Console for Windows, x64 MSVC JUCE, sha256 `102ff5c8…f561`). Static analysis only. Pseudo-C for
every cited address is in `~/wine-ua-work/console-port/rack/decomp/<va>.c` (regenerate with `tools/run_decomp.sh`).
Reference capture: `~/wine-ua-work/console-port/refshots/plugin-window-gtr-ins4.png`. This is Console **1.3.1 on macOS**,
2× scale, 608×1420 px = 304×710 pt, showing "GTR, Insert 4" (SSL G Bus Compressor, HTML GUI).

**Conventions**
- **[obs]** = read from code or data. **[obs-shot]** = measured from the capture (±0.5 pt). **[inf]** = inference. **[unk]** = not determined.
- All values are in pt. Rects are `(x, y, w, h)`, relative to the parent named in the section.
- `W`/`H` is the window size, `cw`/`ch` the content size, `pw`/`ph` the plug-in editor size.
- Images under `Resources/images/uapw/` are @2× art and are drawn at **px/2 pt**. `TextButton::DrawImage` 0x18169ba50 and the 9-slice helper 0x1816969d0 both divide by 2 [obs].
- Image lookup: the code name `foo_png` maps to `images/uapw/foo.png` (RckPw_ImageHelper 0x1815d4370, prefix `uapw/`) [obs].
- Only UA file *names* are listed here. Never copy the assets into the repo.

---------------------------------------------------------------------------------------------------

## 1. Window structure

| level | class / object | rect | notes | tag |
|---|---|---|---|---|
| window | `Rack_PluginWindow` : `Rack_DocumentWindow` : `juce::DocumentWindow` (ctor 0x181624500) | `(0,0,W,H)` | Created as `DocumentWindow("PLUGIN", bg = black 0xff000000, requiredButtons = 4)`, so the only title-bar button is **close**. On Windows it is owned by the main window (`SetWindowLongPtrW(GWLP_HWNDPARENT)`). LookAndFeel is `Rack_LookAndFeel`. `setOpaque(true)`. | obs |
| ↳ border | JUCE `ResizableWindow` border | 1 pt on every side | `getBorderThickness()` returns 1 when there is no resizable border. The capture fits exactly: 304 = 302 + 2 and 710 = 686 + 22 + 2. | inf+obs-shot |
| ↳ title bar | JUCE-drawn title bar, **not native** | `(1, 1, W−2, 22)` | `setTitleBarHeight(22)` is at 0x181628060 (callee 0x181dd1890). Painted by `Rack_LookAndFeel::drawDocumentWindowTitleBar` 0x180cae680 (§5). | obs |
| ↳ LLM icon | `plugin_llm_mode` (skin `Plugin-Window-LLM-Mode-On`, `buttons/icon_llm_row_on.png`, 12×12) | `(W−12−26, (22−12)/2, 12, 12)` | Child of the window, sitting in the title bar to the left of the close button. Visible only when PluginWindowState mode = 5. Positioned in `Rack_PluginWindow::resized` 0x18162e450. | obs |
| ↳ content | `Rack_PluginWindowContent` (id `content`), owned by the window and set with `setContentNonOwned(content, true)` | `(1, 23, cw, ch)` | `paint` 0x18162e2c0 fills **black** (0xff000000, global 0x184c1fd48 set at 0x180867d20). If a cached editor snapshot exists, `paint` draws it at `(1, 33, cw−2, ch−34)`. | obs |
| ↳↳ header | `Rack_NewPlugInHeader` (ctor 0x1816242b0, a subclass of `Rack_PlugInHeader`); builder 0x180df9660, resized 0x180e08890, paint 0x180e08050 | `(0, 0, cw, 32)` | View id `Plug-In-Header`, set in 0x1816297e0 and 0x18162e4d0. | obs |
| ↳↳ multi-out mixer | `Rack_MultiOutputMixer` (only with feature flag 0x184c218e0) | `(0, 32, cw, GetHeightForWidth(cw))` | Shown only for multi-output plug-ins. | obs |
| ↳↳ editor | plug-in editor (native or HTML `UAPW_WebView`) | top-left `(1, 32 + mixerH)`, size `(pw, ph)` | `_UpdateSize` 0x18162ce40 calls `setTopLeftPosition(content.x+1, content.y+mixerH+32)`. The host component at +0x248 gets `(0, 32, cw, ch−32)`, or `ch−72` when the plug-in-manager button is shown. | obs |
| ↳↳ plug-in-manager button | `UAPW::TextButton` "GO TO PLUG-IN MANAGER", id `plugin_mgr_button` (0x181626790) | `((cw−160)/2, ch−40, 160, 26)` | Shown only when the plug-in can't be displayed (state check 0x18162b0d0). Adds 50 to `ch`. | obs |
| ↳↳ footer | `Rack_PlugInFooter` (ctor 0x180df10a0, id `footer`). Added with `addChildComponent` and gated by feature flag 0x184c29570. | `(0, ch−32, cw, 32)` | Visibility follows `ShouldShowFooter` 0x18162a590 (§4). | obs |

**Z-order of content children** [obs 0x181628060 / 0x181626790]:
1. header
2. multi-out mixer
3. footer (hidden until needed)
4. plug-in-manager button
5. editor host (added by `CreateEditor` 0x181626dc0)

The native editor is a separate heavyweight child, so it sits on top [inf].

## 2. Sizing (window ⇄ editor)

| rule | value | tag |
|---|---|---|
| content width | `cw = pw + 2`: a 1-pt black margin on the left and the right | obs 0x18162ce40 |
| content height | `ch = ph + 33` (header 32 + 1-pt bottom margin). Add 32 when the footer shows (`ph + 65`), 50 for the plug-in-manager button, and `mixerH` for the multi-out mixer. | obs 0x18162ce40, inverse in 0x18162c6b0 |
| window size | `W = cw + 2`, `H = ch + 22 + 2` (1-pt border, 22-pt title bar) | inf (JUCE) + obs-shot |
| check against the capture | The SSL G manifest gives `window` 300×621 [obs file `…ssl_g_bus_compressor…/html/manifest.json`]. That yields cw = 302, ch = 621 + 65 = 686, W×H = 304×710 = the capture ✓. The editor sits at abs (2, 55)–(302, 676) ✓. | obs-shot |
| minimum / resize | There is no window-level minimum. `Rack_PluginWindowComponentBoundsConstrainer::checkBounds` 0x18162d6f0 turns window bounds into editor bounds (`−2`, `−33/−65/−50/−mixerH`), lets the editor constrain them (0x181626c30), and adds the chrome back. The window is only resizable when the plug-in allows it (`uapw_allow_resizable_gui*` options, 0x180df6ef0). | obs / inf |
| editor placement | Fixed at content (1, 32+mixerH). No centring: the window always hugs the editor. | obs |
| width-jiggle | When an editor exists, the content is first set to `(pw+3, H+1)` and then to the real size (`uapw_gui_jiggle_hack`) | obs 0x18162ce40 |

## 3. Toolbar (`Rack_PlugInHeader`, height 32)

### 3.1 Background
- **Default** [obs 0x180e08050 → `Rack_Background::paint`]: skin `backgrounds:Plug-In-Header`, which is `backgrounds/plugin_toolbar_bg.png` (400×66 px, retina, `stretch`) stretched over `(0,0,cw,32)`. The art has 1-pt black left, right and bottom edges, so the toolbar looks outlined.
- **HTML ("Mammoth") plug-ins**: when 0x180e06b60 is true, the per-plug-in colour from the GUI `manifest.json` `appearance` block replaces the bitmap (loaded by 0x180dfeb40 and `UpdateMammothGUI` 0x180e04c70).
  - It draws a solid `fillAll(colour)` and then a black 1-pt `drawRect(0, −10, cw, 32+10)`, so only the left, right and bottom edges are visible [obs].
  - Which manifest key it uses is [inf] `window-background`.
  - The capture measures (75,75,75). The SSL manifest has `85,85,85`. The capture's greys are about 12 % darker than the asset values throughout (title art 60→53), so the two agree after colour management [inf].
- `UpdateMammothGUI` also reads `hide_toolbar_preset_management`, which hides the preset group (+0x3aa) [obs].

### 3.2 Buttons: common rendering
- Every element except "more" is a `UAPW::TextButton` (ctor 0x18169b150, mode 0 = image background) [obs].
- **Rect rule:** `y = 5`, `h = 32 − 10 = 22` [obs 0x180e08890].
- **Background:** 9-slice of the named uapw image (0x1816969d0). The corner is `floor(min(imgW, imgH)/3)` px, drawn at half size. Edges and centre are stretched [inf: stretch].
  - `btn_header_bg` is black at α≈0.30 with a darker 1-px rim. Over (75,75,75) it measures (52,52,52) with a (42,42,42) rim ✓.
- **Icon:** drawn centred at `((w−iw/2)/2, (h−ih/2)/2)` with size `iw/2 × ih/2`. An explicit icon rect at +0x28c overrides this. The image is `image-on` when the toggle state is on, otherwise `image-off` (0x18169c080 / 0x18169ba50) [obs].
- **Text:** two `juce::Label`s, Helvetica 10 bold, centred [obs 0x18169b150].
  - Main label at the button bounds, colour **white α0.86** (0x184c1fe94 = 0xffffffff, at 0x180868290).
  - Shadow label at +1/+1, shown when `SetShadow(1)` (0x18169bd90) is called. Its colour is [unk], probably black α0.4 like 3.4.
  - "On" text colour (+0x214) is **(255,197,74) α0.7** [obs 0x18169b150 / 0x181c920b0].
- **Preset field corner arrow:** `SetDropdown` 0x18169bde0 points at `dropdown_arrow_png`, but that file is not in 1.3.1's `images/uapw/`, so nothing is drawn [inf].
- **Hover/pressed:** `UAPW::TextButton::paint` has no hover or pressed variant. The only state is toggle → `image-on` [obs].
- **Background by state** (`SetBgState` 0x18169c970): 2 = `btn_header_error_bg` (red), 3 = `btn_footer_blue`, anything else = `btn_header_bg` [obs].

### 3.3 Layout algorithm [obs 0x180e08890, 0x180df8160, 0x180e03790]

1. **Layout mode** `m` (0x180df8160).
   - `m = 0` when the header is in channel-strip mode (`fn_3b0` 0x180df8080).
   - Otherwise `m` is the smallest value in {0, 1} for which `5 + Σ(shown items)(wᵢ(m) + 5) ≤ cw`. If neither fits, `m = 2`.
   - Item widths `wᵢ(m)`:

     | item | width |
     |---|---|
     | 0, 1, 4, 5 | 22 |
     | 2 (preset group) | 263, or **153** when m = 2 |
     | 3 (sidechain) | 160 when m = 0, otherwise 22 |
     | 6 | 44 |
     | 7 | 80 |
     | 8 | 102 |

2. **Left group.** The cursor starts at `x = 5`.
   - Item 0 goes at `(5, 5, 22, 22)` and moves the cursor to x = 32.
   - Item 1 goes at `(x, 5, 22, 22)`, `x += 27`.
   - Item 2 places prev `(x, 5, 22)`, next `(x+22, 5, 21)` and presets `(x+43, 5, P)`, where `P = 220`, or 110 when m = 2. Then `x += 43 + P + 5`.
3. **Overflow pass.** Items 3, 4, 5, 6, 7 and 8 are placed left to right while `(cw − R − x) − 5 ≥ wᵢ`. After the first item that doesn't fit, it and every later item are hidden.
   - Pass 0 uses `R = 0`. If anything overflowed, pass 1 redoes the placement from the same start x with `R = 27`, or `R = 63` when "more" has the "UAD-2" label.
   - Pass 0 is skipped (go straight to pass 1) when "more" is forced (+0x390 [unk] or the label flag +0x391) or when A/B is shown.
4. **"More"** sits at `(cw − M − 5, 5, M, 22)`, with `M = 22`, or 58 when it has the label. It is visible when it is forced, when anything overflowed, or when A/B is shown.
   - Hidden items stay reachable from its menu (0x180dfb8d0): `50%…200%` GUI scale, `Sidechain`, `Power`, `In`, `Copy A to B`, `Copy`, `Paste`, `Resize Plug-In`, `Apply To All`, `Help & Video`.

**Capture check** [obs-shot, toolbar x = abs − 1]. With cw = 302 the mode is 2 (351 > 302) and "more" carries "UAD-2".

| element | rect | measured | match |
|---|---|---|---|
| ⋮ assign | 5–27 | 5–27 | ✓ |
| ‹ prev | 32–54 | 32–54 | ✓ |
| › next | 54–75 | 54–75 | ✓ |
| preset | 75–185, shows "Default" centred | 75–185 | ✓ |
| power | 190–212 | 190–212 | ✓ |

- A/B (44) and COPY/PASTE no longer fit with R = 63, so they are hidden ✓.
- "UAD-2 •••" sits at 239–297 ✓. Its label starts at +6, and the dots are right-aligned 4 pt from the edge ✓.
- Button tops are at abs y 28 = 23 + 5 ✓, and buttons are 22 tall ✓.

### 3.4 "More" button: `UAPW::Toolbar::ImageButtonWithLabel`
- ctor 0x18169ce80, `paintButton` 0x18169d290. It derives from `juce::ToolbarItemComponent`.
- Background: 9-slice `btn_header_bg` [obs].
- Label (`"UAD-2"` when +0x3a9, otherwise empty): Helvetica 10 bold, left-aligned and vertically centred [obs].
  - Shadow pass at `(+7, +2)`, black α0.4.
  - Main pass at `(+6, +1)`, white α0.86.
- Icon: drawn centred when there is no label, otherwise right-aligned at `x = w − iw/2 − 4`, vertically centred [obs].
  - Toggle on: `icon_more_yellow` (28×8 px). Toggle off: `icon_more` (32×12 px). The on = toggled mapping is [inf].
  - Alpha comes from vslot 0x1b8 (hover/down) [obs], value [unk].

## 4. Footer (`Rack_PlugInFooter`)

| item | rule | tag |
|---|---|---|
| paint 0x180df4710 | `fillAll(UAPW colour id 1)`. The id names come from 0x181693320: 0 `window-background`, 1 `scrollbar-thumbnail`, 2 `list-selection`, 3 `text-link`, 4 `text-selection`, 5 `button-primary`, 6 `button-secondary`; values come from the plug-in's GUI `manifest.json` (`appearance.colors`), so SSL G = 85,85,85. Then a black 1-pt `drawRect(0,0,cw,32)`. | obs |
| resized 0x180df47a0 | `state_button` is 220×22 and `action_button` 72×22, with a 4-pt gap. The visible ones are centred as a group: `x = (cw − total)/2` with total clamped to `cw − 8`, and `y = (32 − 22)/2 = 5`. | obs |
| capture | Plate at abs (42, 682, 220, 22). Relative to the footer that is (41, 5) = (302 − 220)/2 ✓. The text "NO UAD-2 DEVICES FOUND" is centred, and the plate is red `btn_header_error_bg` ✓. | obs-shot |
| state text | Comes from the engine/license layer (0x18190aff0 via `UpdateButtons` 0x180df3630) and is set with `setButtonText`. The background follows `SetBgState(state+1)`. **"NO UAD-2 DEVICES FOUND" is not a literal anywhere in the Windows install** (checked Rack.dll ASCII/UTF-16 and Resources). | obs / inf |
| action text | `BUY` (background `btn_footer_blue`) or `START DEMO` (state 3, blue). `DEMO` is matched in the state string. Clicks go to `DoStateClicked` 0x180df22f0 (ActivateLicense / StartDemo / UAConnect) and `DoActionClicked` 0x180df2040 (`uaconnect://`). | obs |
| visibility | `ShouldShowFooter` 0x18162a590 (license, demo or device state). The exact predicate is [unk]. | obs/unk |

## 5. Title bar

| item | value | tag |
|---|---|---|
| background | Skin `backgrounds:Title-Bar` = `backgrounds/plugin_titlebar.png` (400×42 px, retina), 9-slice drawn over `(0,0,w,22)` | obs 0x180cae680 |
| title text | "Helvetica Neue" 11 bold, colour (229,245,255), centred over the full title-bar width (Justification 0x24). The capture's title is centred at x = 152 = W/2 ✓. | obs + obs-shot |
| title string | `<channel name>` followed by `, Insert <n>`, `, Unison` or `, Rec FX <n>` (`Rack_PluginWindowData::GetTitle` 0x181632090) | obs |
| close button | `Rack_DocumentWindowButton` (factory 0x180cad710, id `close`). `positionDocumentWindowButtons` 0x180cb4400 builds the skin key as `"Small-"` (title height 22) + `"Window-Button-"` + `"Close"` + `"-Win"`, giving **`Small-Window-Button-Close-Win`**: `buttons/titlebar_close_win.png` 16×16, `bounds-background 0,2,20,20`. | obs |
| close rect (Windows) | Right side (JUCE non-Mac default): `(1 + (W−2) − 3 − 20, 1 + 2, 20, 20)` = `(W−24, 3, 20, 20)` in window coordinates (0x180cac270) | inf |
| close (Mac capture) | Left side. Skin `Window-Button-Close` (`icon_x_white.png` 12×12, bg 5,1,20,23). The X glyph is at abs (11–21, 7–17). | obs-shot |
| min/max | Not created (`requiredButtons = 4`). The `Window-Button-Min-Max` skin belongs to other windows (ARA and clip editors). | obs |

## 6. LLM mode (brief)

Gated by an assistant engine object (vslot +0x118). The window then gets a 12×12 `Plugin-Window-LLM-Mode-On` icon (id `plugin_llm_mode`) in the title bar at `(W−38, 5)`. The icon is visible iff PluginWindowState mode == 5, and `InLLMModeChanged` 0x181627e90 toggles it [obs].

## 7. Image sizes (`images/uapw`, px → pt)

| image | px | pt | image | px | pt |
|---|---|---|---|---|---|
| btn_header_bg | 128×44 | 64×22 | icon_prev / icon_next | 16×24 | 8×12 |
| btn_header_bg_l | 44×44 | 22×22 | icon_power / _on | 28×28 | 14×14 |
| btn_header_bg_c | 42×44 | 21×22 | icon_more | 32×12 | 16×6 |
| btn_header_bg_r | 42×44 | 21×22 | icon_more_yellow | 28×8 | 14×4 |
| btn_header_error_bg | 128×44 | 64×22 | icon_more_vertical | 10×36 | 5×18 |
| btn_footer_blue | 87×44 | 43.5×22 | plugin_rack_view / _on | 12×32 | 6×16 |
| current_preset_bg | 784×108 | 392×54 | icon_play_help | 48×48 | 24×24 |

- `current_preset_bg`, `file_operations_bg`, `icon_fav_*`, `icon_play_help`, `preset_browser_*`, `tag_*`, `category_line*` and `line_separator` belong to the UAPW **preset browser** (`UAPW::PresetFileOperations`, `TagsSection`, `HelpPopover`, …), not the window chrome [obs: refs].
- The `views:Plug-In-*-Button` skins (Presets, Copy, Paste, In-Out, Sidechain, Automation, Demo, Buy) have **no code references** in Rack.dll [obs `appearance_xref.tsv`]. They are legacy; the Console header uses the uapw images above.

---------------------------------------------------------------------------------------------------

## Rust implementation table

**Constants.**
- Toolbar: height 32, button `y = 5`, `h = 22`.
- Content background: black.
- Window: `W = pw + 4`, `H = ph + 57`, or `ph + 89` with the footer (+50 for the plug-in-manager button, +mixerH for the mixer).
- Editor at content `(1, 32)`.
- Title bar 22 (window y 1…23); window border 1.
- Text: Helvetica 10 bold, white α0.86, on colour (255,197,74,α0.7). Title: Helvetica Neue 11 bold (229,245,255), centred.

| # | id | rect rule (toolbar coords) | bg image | icon / label off → on | shown when | action |
|---|---|---|---|---|---|---|
| 0 | `channel_strip` | `(5,5,22,22)`, then x = 32 | btn_header_bg | `plugin_rack_view` → `plugin_rack_view_on` | feature flag 0x184c2a790 (off in capture) | toggle rack-controls / plug-in GUI view |
| 1 | `assign` | `(x,5,22,22)`, x += 27; icon rect `(9,2,5,18)` | btn_header_bg | `icon_more_vertical` | not channel-strip mode | insert menu (INSERT / OPTIONS / Assign…) 0x180dff8f0 |
| 2a | `prev_preset` | `(x,5,22,22)` | btn_header_bg_l | `icon_prev` | preset mgmt not hidden | previous preset (0x180e01b70) |
| 2b | `next_preset` | `(x+22,5,21,22)` | btn_header_bg_c | `icon_next` | same | next preset |
| 2c | `presets` | `(x+43,5,P,22)`, P = 220 or 110 (m = 2); x += 43+P+5 | btn_header_bg_r | text = preset name or "Default" (italic when modified [inf]) | same | open preset browser (0x180e03ec0) |
| 3 | `sidechain` | `(x,5,S,22)`, S = 160 (m = 0) or 22; x += S+5 | btn_header_bg | `SIDECHAIN: {input}` or `SC`; on colour amber | effect has a sidechain | sidechain source picker (0x180e03020) |
| 4 | `power` | `(x,5,22,22)`, x += 27 | btn_header_bg | `icon_power` → `icon_power_on` (toggle) | !channel-strip && 0x180e01f20 | bypass toggle (0x180dff350) |
| 5 | `in_out` | `(x,5,22,22)`, x += 27 | btn_header_bg | `IN` | 0x180e019c0 | in/out toggle (0x180dfe900) |
| 6 | `a` / `b` | `(x,5,22,22)` and `(x+22,5,22,22)`, x += 49 | btn_header_bg_l / btn_header_bg_r | `A` / `B` | flag 0x184c2e8b0 and the power rule | A/B compare [unk handler] |
| 7 | `copy` / `paste` | `(x,5,40,22)` and `(x+40,5,40,22)`, x += 85 | btn_header_bg_l / btn_header_bg_r | `COPY` / `PASTE` | power rule | copy 0x180df92c0 / paste 0x180e02630 |
| 8 | `mixer` | `(x,5,102,22)` | btn_header_bg | `MULTI-OUT MIXER` | flag 0x184c218e0 and a multi-out effect | toggle multi-out mixer |
| 9 | `more` | `(cw−M−5,5,M,22)`, M = 22 or 58 with "UAD-2" | btn_header_bg | `icon_more` → `icon_more_yellow`; label "UAD-2" for UAD-2 plug-ins | overflow / forced / A-B shown | menu: scale 50–200 %, overflowed items, Resize, Apply To All, Help & Video |
| F1 | footer `state_button` | footer `(cx,5,220,22)`, centred with F2 (gap 4) | by state: btn_header_bg / **btn_header_error_bg** (2) / btn_footer_blue (3) | engine license/status text | footer shown | ActivateLicense / StartDemo |
| F2 | footer `action_button` | `(cx+224,5,72,22)` | btn_footer_blue | `BUY` / `START DEMO` | as UpdateButtons | open uaconnect:// |
| T1 | title close | Windows `(W−24,3,20,20)`; Mac on the left | skin `Small-Window-Button-Close-Win` (titlebar_close_win.png) | — | always | close window |
| T2 | `plugin_llm_mode` | `(W−38,5,12,12)` | skin `Plugin-Window-LLM-Mode-On` | — | LLM mode = 5 | — |

Overflow uses two passes over items 3→8 against `cw − R − 5`, with R ∈ {0, 27, 63}; see §3.3.

## Open items [unk]
- `Rack_NewPlugInHeader` overrides (vslots 3–5 of `Rack_PlugInHeader` 0x180df7480 / 0x180df79a0 / 0x180df7a80 vs the subclass); the builder, resized and paint are shared.
- The "more forced" flag (+0x3a8 / +0x390), the predicates of `fn_3b0` (channel-strip mode), 0x180e01f20, 0x180e019c0 and `ShouldShowFooter`, and the A/B click handler.
- Text-shadow colour of `UAPW::TextButton`, hover alpha of "more", and edge-stretch vs tile in the 9-slice helper.
- The exact manifest key used for the HTML-GUI toolbar tint, and the true sRGB values (the capture is colour-managed).
- Windows font fallback for "Helvetica" / "Helvetica Neue". Glyph heights in the capture (cap ≈ 8.5 pt for "Default") look larger than 10 pt Helvetica.
