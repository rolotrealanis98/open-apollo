/*
 * uaconsole.h — C API of console-rs (crates/uaconsole-ffi), for native Apple hosts.
 *
 * Hand-written; keep in sync with crates/uaconsole-ffi/src/lib.rs (its `abi` test checks
 * the UacStats and UacPluginRequest layouts). Rendering goes through wgpu's Metal backend into
 * a CAMetalLayer the host owns. UA's art is never bundled: the host passes the path of a UAD
 * Console `Resources` folder (on iPad, one the user imported into Documents).
 *
 * Threading: a context is single-threaded. Create, drive and destroy it on one thread (the
 * main thread on iOS, where the CAMetalLayer and the CADisplayLink live). An engine
 * connection runs on its own threads; uac_needs_frame()/uac_frame() apply its updates.
 *
 * Coordinates: pointer and zoom anchors are in view points (UIKit/AppKit points, origin at
 * the layer's top-left). The context owns the mapping to Console window points.
 *
 * Layout: like Console, the full layout follows its window. By default the Console window
 * is the view's size in points (uac_resize), so a 1470 x 1105 pt view lays out exactly like
 * a 1470 x 1105 pt Console window at zoom 1; uac_set_window_size() pins another size.
 *
 * Engine safety: uac_connect_engine() is read-only unless `writes` is true. Read-only, the
 * connection refuses set/post and controls move locally only.
 *
 * SPDX-License-Identifier: GPL-2.0-only
 */
#ifndef UACONSOLE_H
#define UACONSOLE_H

#include <stdbool.h>
#include <stdint.h>

#ifdef __cplusplus
extern "C" {
#endif

/* Return codes. */
#define UAC_OK               0
#define UAC_SKIPPED          1   /* frame skipped (surface timeout/occluded/reconfigured) */
#define UAC_ERR_NULL        (-1)
#define UAC_ERR_SURFACE     (-2)
#define UAC_ERR_UNSUPPORTED (-3)
#define UAC_ERR_IO          (-4)
#define UAC_ERR_ARG         (-5)
#define UAC_ERR_PANIC       (-99)

/* uac_pointer kinds. */
#define UAC_POINTER_DOWN   0
#define UAC_POINTER_MOVE   1
#define UAC_POINTER_UP     2
#define UAC_POINTER_CANCEL 3
#define UAC_POINTER_SECONDARY 4 /* right click / long press: Console's context menus */
#define UAC_POINTER_HOVER  5 /* motion with no button down: highlights an open menu's rows */

/* uac_pointer flags. */
#define UAC_POINTER_DOUBLE (1u << 0) /* double tap: reset a dragged control to its default */
#define UAC_POINTER_FINE   (1u << 1) /* fine adjustment: motion / 10; while dragging an insert: copy (Option) */
/* The pointer is a touch. UAC_POINTER_SECONDARY (long press) with this flag on a loaded insert or a
 * browser plug-in row picks it up: send the long press's later positions as UAC_POINTER_MOVE and
 * its end as UAC_POINTER_UP (with this flag) to drag and drop it; a long press released without
 * moving opens the context menu. Mouse: press, move 4 pt and release drags; no flag needed. */
#define UAC_POINTER_TOUCH  (1u << 2)

/* uac_set_model_engine flags. */
#define UAC_ENGINE_WRITES  (1u << 0) /* open both write gates (only after the user asked) */

/* uac_model_state values. */
#define UAC_MODEL_PREVIEW      0
#define UAC_MODEL_SNAPSHOT     1
#define UAC_MODEL_CONNECTING   2
#define UAC_MODEL_LOADING      3
#define UAC_MODEL_LIVE         4
#define UAC_MODEL_RECONNECTING 5
#define UAC_MODEL_FAILED       6
#define UAC_MODEL_OFFLINE      7

/* uac_key keys. */
#define UAC_KEY_ENTER     1
#define UAC_KEY_BACKSPACE 2
#define UAC_KEY_ESCAPE    3

/* UacPluginRequest.slot for the Unison slot. */
#define UAC_UNISON_SLOT 255

typedef struct UacContext UacContext;

typedef struct UacStats {
    uint64_t frames;          /* frames presented since creation */
    double   build_ms;        /* last frame: draw list + hit regions (CPU) */
    double   render_ms;       /* last frame: encode/upload/submit (CPU, excludes GPU time) */
    uint32_t draws;
    uint32_t hits;
    uint32_t quads;
    uint32_t batches;
    uint32_t textures;
    uint32_t glyphs;
    uint64_t texture_bytes;
    float    content_width_pt;  /* Console window size in Console points */
    float    content_height_pt;
    float    zoom;              /* view points per Console point */
    float    origin_x_pt;       /* Console point at the view's top-left */
    float    origin_y_pt;
} UacStats;

/* A loaded insert (or Unison) slot the user opened: the host shows its plug-in window.
 * Strings are owned by the context: valid until the next uac_take_plugin_request() or
 * uac_destroy(). */
typedef struct UacPluginRequest {
    const char *strip_id;     /* engine node path ("/devices/0/inputs/1") or "in1" (preview) */
    const char *strip_title;  /* as the mixer shows it ("ANALOG 1") */
    const char *slot_path;    /* the slot's engine node ("…/inserts/2"); "" without an engine */
    const char *effect_name;  /* the loaded plug-in's EffectName */
    int32_t     slot;         /* 0-based insert slot, or UAC_UNISON_SLOT */
} UacPluginRequest;

/* One of Console's separate windows (uac_set_windowing): a popup menu level, the Settings
 * window, a panel, a dialog. Rect in view points from the view's top-left (may lie outside
 * the view). id: stable while the window is open; valid until the next frame. */
typedef struct UacLayer {
    const char *id;           /* "menu:0", "settings", "panel:cue-outputs", ... */
    int32_t     kind;         /* UAC_LAYER_* */
    float       x, y, width, height;
    float       title_height; /* Console's drawn title bar: a press there moves the window */
} UacLayer;

#define UAC_LAYER_MENU    0   /* borderless, non-activating, above everything */
#define UAC_LAYER_WINDOW  1   /* free window (Settings, Monitor, ...): movable, remembered */
#define UAC_LAYER_DIALOG  2   /* dialog centred on the screen (ADD DEVICE) */
#define UAC_LAYER_POPOVER 3   /* callout anchored to its control (TEMPO, Sends, Cues) */

/* Library version (static string). */
const char *uac_version(void);

/* Last error on this thread, "" if none. Valid until the next failing call on this thread. */
const char *uac_last_error(void);

/* True when `path` is a UAD Console Resources folder (contains appearance/Appearance.json). */
bool uac_resources_valid(const char *path);

/*
 * Creates a context.
 *   resources_path  a UAD Console `Resources` folder, or NULL to search the default
 *                   locations ($CONSOLE_RESOURCES, the macOS app bundle, …).
 *   metal_layer     a `CAMetalLayer *` (bridged, unretained) that outlives the context, or
 *                   NULL to render offscreen (tests; see uac_write_png).
 *   width_px, height_px  drawable size in pixels.
 *   scale           pixels per view point (UIScreen scale / traitCollection.displayScale).
 * Returns NULL on failure; see uac_last_error().
 */
UacContext *uac_create(const char *resources_path, void *metal_layer,
                       uint32_t width_px, uint32_t height_px, float scale);

/* New drawable size (pixels) and pixels per view point. Unless a window size is pinned, the
 * Console window follows the view and the full layout reflows (Console's responsive window). */
void uac_resize(UacContext *ctx, uint32_t width_px, uint32_t height_px, float scale);

/* Pins the Console window size in Console points (e.g. 1920 x 1050, Console's default);
 * width or height <= 0 follows the view again (the default). The view fits the window unless
 * zoomed; the layout never goes below its minimum size. */
void uac_set_window_size(UacContext *ctx, float width_pt, float height_pt);

/* The host went to the background (true) or came back (false): the engine connection sends
 * Console's per-socket `set /Sleep <bool>`. */
void uac_set_background(UacContext *ctx, bool background);

/* Call every display refresh: applies queued engine updates and returns true when a frame is
 * due (model, view or input changed, or live meters moved; meters redraw at <= 30 Hz). */
bool uac_needs_frame(UacContext *ctx);

/* Forces the next uac_needs_frame() to return true (layer contents lost, benchmarks). */
void uac_invalidate(UacContext *ctx);

/* Applies queued engine updates, builds the draw list from the model and renders one frame
 * into the layer. Returns UAC_OK, UAC_SKIPPED or a negative error. */
int32_t uac_frame(UacContext *ctx);

/* Pointer/touch event in view points. DOWN/MOVE/UP/CANCEL drive rack's Interaction through
 * the mixer's ViewState (navigator, browser, popovers, modifiers, then the model); drags off
 * controls pan a zoomed view; SECONDARY opens context menus. Returns 1 if it hit or is
 * driving a control, 0 if it panned the view or missed, or a negative error. Track one
 * touch; send CANCEL when a gesture recognizer takes over. Tap-kind controls (inserts, menu
 * items, browser rows) fire on UP inside them, so a long press (DOWN, CANCEL, SECONDARY)
 * opens the context menu without opening the plug-in. A tap on a loaded insert queues a
 * plug-in request (uac_take_plugin_request). */
int32_t uac_pointer(UacContext *ctx, int32_t kind, float x_pt, float y_pt, uint32_t flags);

/* True while a pointer drag drives a control (fader, knob): keep a long-press recognizer from
 * turning a held drag into a context click. */
bool uac_dragging(UacContext *ctx);

/* Wheel/trackpad scroll at a point; deltas in view points, AppKit's scrollingDelta convention
 * (positive reveals content above / to the left). Scrolls the browser list or the cells
 * column under the point (returns 1), else pans a zoomed view (returns 0). */
int32_t uac_scroll(UacContext *ctx, float x_pt, float y_pt, float dx_pt, float dy_pt);

/* Keyboard for a focused text field (channel rename, tempo, browser search). */
bool uac_text_editing(UacContext *ctx);
/* UTF-8 text; returns 1 if a field took it. */
int32_t uac_key_text(UacContext *ctx, const char *utf8);
/* UAC_KEY_ENTER / BACKSPACE (focused field), ESCAPE (closes field, popover, sheet or
 * modifier). Returns 1 if consumed. */
int32_t uac_key(UacContext *ctx, int32_t key);

/* Zoom in view points per Console point, keeping the Console point under the anchor (view
 * points) in place. zoom <= 0 returns to fit-to-view. Clamped to [fit, 4]. */
void uac_set_zoom(UacContext *ctx, float zoom, float anchor_x_pt, float anchor_y_pt);

/* Compact layout: hides the plug-in browser/navigator, packs aux + monitor after the strips. */
void uac_set_compact(UacContext *ctx, bool compact);

/* Offline preview mixer (6 inputs, 2 aux). Edits stay local. This is the default model.
 * Closes an engine connection. */
void uac_set_model_preview(UacContext *ctx);

/*
 * Connects to a UA Mixer Engine (UA's engine or the open-apollo daemon) at host:port
 * (port 0 = 4710) and shows it live (engine_model::EngineMixer over ua-engine, plus the
 * read-only UA Mixer Helper on host:4720 when port is 4710). Replaces the model, closing an
 * earlier connection. Connecting runs in the background; see uac_model_state().
 * READ-ONLY unless `writes`: the connection refuses set/post and controls move locally only.
 * Pass writes=true only after the user explicitly enabled writes.
 */
int32_t uac_connect_engine(UacContext *ctx, const char *host, uint16_t port, bool writes);

/* uac_connect_engine() with flags (UAC_ENGINE_WRITES). */
int32_t uac_set_model_engine(UacContext *ctx, const char *host, uint16_t port, uint32_t flags);

/* Shows an engine snapshot offline (ConsoleLab/ua-engine snapshot JSON, a raw
 * `get /?recursive=1` reply or an open-apollo device map). Edits stay local. */
int32_t uac_use_snapshot(UacContext *ctx, const char *path);

/* UAC_MODEL_* (negative on error). */
int32_t uac_model_state(UacContext *ctx);

/* One status line, e.g. "Apollo x4 · live · read-only". Owned by the context; valid until
 * the next call. */
const char *uac_model_status(UacContext *ctx);

/* Takes the oldest pending plug-in window request into *out; false when there is none. */
bool uac_take_plugin_request(UacContext *ctx, UacPluginRequest *out);

/* Console's menu bar as JSON for native menus: {"app": [entries], "menus": [{"title",
 * "items": [entries]}]}. An entry is {"type":"item","label","command","enabled","ticked",
 * "shortcut":{"key","command","shift","option","control"}|null}, {"type":"separator"},
 * {"type":"header","label"} or {"type":"submenu","label","enabled","items"}. "app" holds the
 * macOS application menu's extra items when `mac`. Valid until the next call or uac_destroy();
 * rebuild native menus when they open. NULL on error. */
const char *uac_menu_bar_json(UacContext *ctx, bool mac);

/* Runs a menu command by id (an item's "command", e.g. "show_browser", "undo"). 1 = ran,
 * 0 = unknown or disabled now. Shell-level commands come back through uac_take_command(). */
int32_t uac_run_command(UacContext *ctx, const char *command_id);

/* The oldest command the menus left for the host ("quit", "toggle_full_screen",
 * "open_documentation_url_app", "select_new_session_panel", "open_session", "save_bookmark",
 * "save_as", ...), or NULL. Valid until the next call or uac_destroy(). */
const char *uac_take_command(UacContext *ctx);

/* The oldest URL the console wants opened, or NULL: a file:// URL from Show In Finder (reveal
 * it in Finder), documentation, Dante Controller (launch the app when installed, else open the
 * page), Dante patents, feedback. Valid until the next call or uac_destroy. */
const char *uac_take_open_url(UacContext *ctx);

/* Keeps Console's client preferences (OPTIONS, MIDI) in the JSON file at path (loaded now, saved
 * on each change); NULL keeps them in memory. */
void uac_set_prefs_path(UacContext *ctx, const char *path);

/* Draws Windows Console's in-window menu bar in the title bar (hosts without a native menu
 * bar). Off by default. */
void uac_set_menu_bar(UacContext *ctx, bool on);

/* Copies the latest stats into *out. */
int32_t uac_get_stats(UacContext *ctx, UacStats *out);

/* GPU adapter name and backend, e.g. "Apple M2 (Metal)". Owned by the context. */
const char *uac_adapter_name(UacContext *ctx);

/* Renders the current view offscreen into an RGBA PNG plus <name>.draws.json (what was drawn
 * where, for console-rs/tools/align). Debugging, tests, alignment. */
int32_t uac_write_png(UacContext *ctx, const char *path);

/* Separate windows (macOS child windows). Console shows its popup menus, Settings window,
 * panels and dialogs as windows of their own, placed on the screen (menus hang past the
 * mixer window). uac_set_windowing turns that on with the screen's usable area
 * (NSScreen.visibleFrame) in view points from the view's top-left, y down; a zero size turns
 * it off and everything is drawn inside the view (the default; the iPad keeps it off).
 * Each frame: uac_layer_count / uac_layer_info, keep one borderless host window per id at
 * view origin + rect (UAC_LAYER_MENU non-activating, above all), give it a CAMetalLayer with
 * uac_attach_layer, route its input through uac_layer_pointer (window-local view points),
 * report title-bar moves with uac_layer_moved; uac_frame draws the view and every attached
 * window. Close host windows whose id is gone (uac_detach_layer). */
int32_t uac_set_windowing(UacContext *ctx, float x, float y, float width, float height);
int32_t uac_layer_count(UacContext *ctx);
bool uac_layer_info(UacContext *ctx, uint32_t index, UacLayer *out);
int32_t uac_attach_layer(UacContext *ctx, const char *id, void *metal_layer, uint32_t width_px, uint32_t height_px);
void uac_detach_layer(UacContext *ctx, const char *id);
int32_t uac_layer_pointer(UacContext *ctx, const char *id, int32_t kind, float x_pt, float y_pt, uint32_t flags);
void uac_layer_moved(UacContext *ctx, const char *id, float x_pt, float y_pt);

/* Destroys a context (NULL is ignored) and closes its engine connection. The layer may be
 * released afterwards. */
void uac_destroy(UacContext *ctx);

#ifdef __cplusplus
}
#endif

#endif /* UACONSOLE_H */
