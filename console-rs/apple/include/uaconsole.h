/*
 * uaconsole.h — C API of console-rs (crates/uaconsole-ffi), for native Apple hosts.
 *
 * Hand-written; keep in sync with crates/uaconsole-ffi/src/lib.rs (its `abi` test checks
 * the UacStats layout). Rendering goes through wgpu's Metal backend into a CAMetalLayer the
 * host owns. UA's art is never bundled: the host passes the path of a UAD Console
 * `Resources` folder (on iPad, one the user imported into Documents).
 *
 * Threading: a context is single-threaded. Create, drive and destroy it on one thread (the
 * main thread on iOS, where the CAMetalLayer and the CADisplayLink live).
 *
 * Coordinates: pointer and zoom anchors are in view points (UIKit points, origin at the
 * layer's top-left). The context owns the mapping to Console window points.
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
#define UAC_ERR_PANIC       (-99)

/* uac_pointer kinds. */
#define UAC_POINTER_DOWN   0
#define UAC_POINTER_MOVE   1
#define UAC_POINTER_UP     2
#define UAC_POINTER_CANCEL 3

/* uac_pointer flags. */
#define UAC_POINTER_DOUBLE (1u << 0) /* double tap: reset a dragged control to its default */
#define UAC_POINTER_FINE   (1u << 1) /* fine adjustment: motion / 10 */

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

/* New drawable size (pixels) and pixels per view point. */
void uac_resize(UacContext *ctx, uint32_t width_px, uint32_t height_px, float scale);

/* True when the model, view or input changed since the last presented frame. */
bool uac_needs_frame(UacContext *ctx);

/* Forces the next uac_needs_frame() to return true (layer contents lost, benchmarks). */
void uac_invalidate(UacContext *ctx);

/* Builds the draw list from the model and renders one frame into the layer.
 * Returns UAC_OK, UAC_SKIPPED or a negative error. */
int32_t uac_frame(UacContext *ctx);

/* Pointer/touch event in view points. Returns 1 if it hit or is driving a control, 0 if it
 * panned the view or missed, or a negative error. Track one touch; send CANCEL when a
 * gesture recognizer takes over. */
int32_t uac_pointer(UacContext *ctx, int32_t kind, float x_pt, float y_pt, uint32_t flags);

/* Zoom in view points per Console point, keeping the Console point under the anchor (view
 * points) in place. zoom <= 0 returns to fit-to-view. Clamped to [fit, 4]. */
void uac_set_zoom(UacContext *ctx, float zoom, float anchor_x_pt, float anchor_y_pt);

/* Compact layout: hides the plug-in browser/navigator, packs aux + monitor after the strips. */
void uac_set_compact(UacContext *ctx, bool compact);

/* Offline preview mixer (6 inputs, 2 aux). Edits stay local. This is the default model. */
void uac_set_model_preview(UacContext *ctx);

/*
 * TODO(engine-model): attach a MixerModel over ua-engine connected to a UA Mixer Engine
 * (open-apollo daemon or UA's engine) at host:port, read-only unless `flags` enables
 * writes. Currently returns UAC_ERR_UNSUPPORTED and leaves the model unchanged.
 */
int32_t uac_set_model_engine(UacContext *ctx, const char *host, uint16_t port, uint32_t flags);

/* Copies the latest stats into *out. */
int32_t uac_get_stats(UacContext *ctx, UacStats *out);

/* GPU adapter name and backend, e.g. "Apple M2 (Metal)". Owned by the context. */
const char *uac_adapter_name(UacContext *ctx);

/* Renders the current view offscreen into an RGBA PNG (debugging, tests). */
int32_t uac_write_png(UacContext *ctx, const char *path);

/* Destroys a context (NULL is ignored). The layer may be released afterwards. */
void uac_destroy(UacContext *ctx);

#ifdef __cplusplus
}
#endif

#endif /* UACONSOLE_H */
