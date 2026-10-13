//! The whole catalog, headless: every spec of the found folder decodes, builds its panel (graph
//! rasters included) with mock parameters at defaults, all-zero and all-one values and with mock
//! meters (needles, raw values, analyser blocks), and survives a tap and a drag on every hit
//! region, a sweep of drags over the panel, its pop-ups and its "more" panel, with clip stacks
//! balanced. Skipped (with a note) when no spec folder is found.

use std::sync::Arc;

use houston::{Appearance, ImageCache, Rect, Resources};
use plugin_ui::{MockParams, ParamSource, PluginPanel, PluginSpec, PluginSpecLibrary};
use rack::{Assets, Ui};

const SLOT: &str = "/devices/0/inputs/0/effects/3";

fn assets() -> Assets {
    let resources = Resources { root: std::env::temp_dir().join("plugin-ui-catalog-no-console"), console_version: None };
    Assets::from_parts(resources.clone(), Appearance::default(), Arc::new(ImageCache::new(resources)))
}

/// Mock meters: every needle and raw value set, analyser bins filled with a falling slope.
fn mock_meters(panel: &mut PluginPanel, level: f64) {
    let spec = panel.spec.clone();
    for id in spec.meter_sources.iter().flatten().map(|(k, _)| k.clone()) {
        panel.meters.set_needle(&id, level);
        panel.meters.set_raw(&id, level * 0.5);
    }
    for g in spec.elements.iter().filter_map(|e| e.graph.as_ref()) {
        if let Some(sp) = &g.spectrum {
            let bins: Vec<f64> = (0..sp.count.max(0)).map(|i| level * 1e-3 / (1.0 + i as f64)).collect();
            panel.meters.set_block(sp.first, &bins);
        }
    }
}

fn build(panel: &PluginPanel, assets: &Assets, params: &dyn ParamSource, stem: &str) -> Vec<rack::Hit> {
    let mut ui = Ui::new(assets);
    let (w, h) = panel.size();
    panel.build(&mut ui, Rect::new(0.0, 0.0, w, h), params);
    assert!(ui.current_clip().is_none(), "{stem}: unbalanced clip stack");
    assert!(!ui.list.is_empty(), "{stem}: nothing drawn");
    ui.hits
}

fn exercise(spec: &Arc<PluginSpec>, lib: &PluginSpecLibrary, assets: &Assets, stem: &str) -> usize {
    let mut params = MockParams::from_spec(spec);
    let mut panel = PluginPanel::new(spec.clone(), lib, SLOT);
    mock_meters(&mut panel, 0.7);
    let mut t = 0.0;
    let hits = build(&panel, assets, &params, stem);
    let mut gestures = 0;
    // A tap, a long press and a drag on every hit region (capped), rebuilding as values change.
    for (n, hit) in hits.iter().take(160).enumerate() {
        let (x, y) = hit.rect.center();
        for (dx, dy, held) in [(0.0, 0.0, 0.05), (0.0, 0.0, 0.6), (18.0, -24.0, 0.2)] {
            if panel.pointer_down_at(x, y, false, t, &mut params) {
                panel.pointer_move_at(x + dx, y + dy, t + held / 2.0, &mut params);
                panel.tick_at(t + held, &mut params);
                panel.pointer_up_at(x + dx, y + dy, t + held, &mut params);
                gestures += 1;
            }
            t += 1.0;
        }
        if n % 8 == 7 {
            build(&panel, assets, &params, stem);
        }
    }
    // A sweep of drags over the panel (graph handles, swipes) and the pop-ups they open.
    let (w, h) = panel.size();
    for gy in 0..6 {
        for gx in 0..6 {
            build(&panel, assets, &params, stem);
            let (x, y) = (w * (gx as f32 + 0.5) / 6.0, h * (gy as f32 + 0.5) / 6.0);
            if panel.pointer_down_at(x, y, false, t, &mut params) {
                panel.pointer_move_at(x + 60.0, y - 30.0, t + 0.1, &mut params);
                panel.pointer_up_at(x + 60.0, y - 30.0, t + 0.2, &mut params);
                gestures += 1;
            }
            t += 1.0;
        }
    }
    panel.show_more(true);
    build(&panel, assets, &params, stem);
    panel.show_more(false);
    // Touch-state timeouts, blinks and spins over time.
    for k in 0..4 {
        t += 1.7;
        panel.tick_at(t, &mut params);
        let _ = k;
    }
    build(&panel, assets, &params, stem);
    // Extreme values and states, mono layout, meters at rest.
    for v in [0.0, 1.0] {
        params.set_all(v);
        let mut p = PluginPanel::new(spec.clone(), lib, SLOT);
        mock_meters(&mut p, v);
        for (state, _) in spec.states.iter().flatten() {
            p.ui_state.insert(state.clone(), v * 2.0);
        }
        p.set_channel_layout(v > 0.5);
        build(&p, assets, &params, stem);
    }
    gestures
}

#[test]
fn every_catalog_spec_builds_and_survives_every_gesture() {
    let Some(lib) = PluginSpecLibrary::locate() else {
        eprintln!("skipped: no plug-in spec folder (set UACONSOLE_PLUGIN_SPECS)");
        return;
    };
    let assets = assets();
    lib.attach(&assets.images);
    let mut total = 0;
    let mut graphs = 0;
    for entry in &lib.entries {
        let spec = lib.spec(entry).unwrap_or_else(|e| panic!("{e}"));
        graphs += spec.elements.iter().filter(|e| e.graph.is_some()).count();
        total += exercise(&spec, &lib, &assets, entry.stem());
    }
    assert!(assets.images.missing().iter().all(|k| !k.starts_with("spec:")), "missing panel sprites: {:?}", assets.images.missing());
    eprintln!("{} specs built and exercised ({} gestures, {} graph elements)", lib.entries.len(), total, graphs);
}
