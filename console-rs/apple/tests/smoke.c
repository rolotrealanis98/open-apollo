/*
 * C smoke test of uaconsole.h against the macOS slice of UAConsoleFFI.xcframework:
 * offscreen context (no layer), first + warm frames, a tap, a pinch-zoom, a PNG.
 *
 *   clang -std=c11 -Wall -Wextra -I apple/include apple/tests/smoke.c \
 *     apple/build/UAConsoleFFI.xcframework/macos-arm64/libuaconsole_ffi.a \
 *     -framework Metal -framework QuartzCore -framework CoreGraphics -framework Foundation \
 *     -lobjc -liconv -o apple/build/smoke
 *   apple/build/smoke [out.png] [warm-frames]
 *
 * Resources come from the default search ($CONSOLE_RESOURCES, the macOS UAD Console app).
 */
#include <stdio.h>
#include <stdlib.h>

#include "uaconsole.h"

static void print_stats(const char *label, const UacStats *s) {
    printf("%s: build %.3f ms, render %.3f ms, %u draws, %u hits, %u quads, %u batches, "
           "%u textures (%.1f MB), %u glyphs; content %.1f x %.1f pt, zoom %.3f\n",
           label, s->build_ms, s->render_ms, s->draws, s->hits, s->quads, s->batches, s->textures,
           (double)s->texture_bytes / 1e6, s->glyphs, s->content_width_pt, s->content_height_pt,
           s->zoom);
}

int main(int argc, char **argv) {
    const char *out = argc > 1 ? argv[1] : "smoke.png";
    int warm = argc > 2 ? atoi(argv[2]) : 120;
    printf("uaconsole-ffi %s\n", uac_version());

    /* iPad Pro 13" landscape drawable: 1376 x 1032 pt at 2x. */
    UacContext *ctx = uac_create(NULL, NULL, 2752, 2064, 2.0f);
    if (!ctx) {
        fprintf(stderr, "uac_create: %s\n", uac_last_error());
        return 1;
    }
    printf("adapter: %s\n", uac_adapter_name(ctx));
    uac_set_compact(ctx, true);

    UacStats s;
    if (uac_frame(ctx) != UAC_OK || uac_get_stats(ctx, &s) != UAC_OK) {
        fprintf(stderr, "first frame: %s\n", uac_last_error());
        return 1;
    }
    print_stats("first frame", &s);

    double build = 0, render = 0;
    for (int i = 0; i < warm; i++) {
        uac_invalidate(ctx);
        if (uac_frame(ctx) != UAC_OK) {
            fprintf(stderr, "frame %d: %s\n", i, uac_last_error());
            return 1;
        }
        uac_get_stats(ctx, &s);
        build += s.build_ms;
        render += s.render_ms;
    }
    if (warm > 0) {
        printf("warm avg over %d frames: build %.3f ms, render %.3f ms (CPU side)\n", warm,
               build / warm, render / warm);
    }

    /* A drag that starts off-control pans (returns 0); the view stays valid. */
    int down = uac_pointer(ctx, UAC_POINTER_DOWN, 4.0f, 4.0f, 0);
    int move = uac_pointer(ctx, UAC_POINTER_MOVE, 40.0f, 40.0f, UAC_POINTER_FINE);
    int up = uac_pointer(ctx, UAC_POINTER_UP, 40.0f, 40.0f, 0);
    printf("pointer: down %d, move %d, up %d, needs_frame %d\n", down, move, up,
           uac_needs_frame(ctx));
    uac_set_zoom(ctx, 1.5f, 688.0f, 516.0f);
    uac_get_stats(ctx, &s);
    printf("zoomed: %.2f, origin %.1f, %.1f pt\n", s.zoom, s.origin_x_pt, s.origin_y_pt);
    uac_set_zoom(ctx, 0.0f, 0.0f, 0.0f);

    /* An engine snapshot offline (UAC_SNAPSHOT=path; never a live engine from a smoke test). */
    const char *snapshot = getenv("UAC_SNAPSHOT");
    if (snapshot && *snapshot) {
        if (uac_use_snapshot(ctx, snapshot) != UAC_OK || uac_model_state(ctx) != UAC_MODEL_SNAPSHOT) {
            fprintf(stderr, "snapshot: %s\n", uac_last_error());
            return 1;
        }
        printf("model: %s\n", uac_model_status(ctx));
    }
    if (uac_connect_engine(ctx, "", 0, false) != UAC_ERR_ARG) {
        fprintf(stderr, "empty host should be refused\n");
        return 1;
    }
    if (uac_write_png(ctx, out) != UAC_OK) {
        fprintf(stderr, "write_png: %s\n", uac_last_error());
        return 1;
    }
    printf("wrote %s\n", out);
    uac_destroy(ctx);
    return 0;
}
