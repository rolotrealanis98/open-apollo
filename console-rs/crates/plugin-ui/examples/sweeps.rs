//! Parameter sweeps against the live AU editor: the spec pipeline's request shots
//! (`$PLUGIN_UI_LIVE_REFS/r_renders/<request>/index.json` + @2x crops, or `$PLUGIN_UI_SWEEPS`;
//! maintainers only, not in this repository) are re-drawn by the spec panel at the same parameter values and crop,
//! and compared pixel by pixel (largest channel difference > 24/255).
//!
//!   cargo run -p plugin-ui --example sweeps [-- <request-name-filter>] [--every N] [--out DIR]
//!
//! Other parameters take the AU's initial values (agent F's dumps), meters read 0, as in the
//! shots. Shots taken with audio are skipped. Prints one line per request: shots, median and
//! worst share of pixels off. `--out` writes ours / theirs for the worst shot of each request.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use houston::{Appearance, ImageCache, ImageData, Rect, Resources};
use plugin_ui::{MockParams, ParamSource, PluginPanel, PluginSpecLibrary};
use rack::{Assets, Ui};

fn arg(name: &str) -> Option<String> {
    let args: Vec<String> = std::env::args().collect();
    args.iter().position(|a| a == name).and_then(|i| args.get(i + 1).cloned())
}

struct Strings {
    inner: MockParams,
    strings: HashMap<u16, String>,
}

impl ParamSource for Strings {
    fn count(&self) -> usize {
        self.inner.count()
    }
    fn name(&self, k: u16) -> Option<String> {
        self.inner.name(k)
    }
    fn normalized(&self, k: u16) -> Option<f64> {
        self.inner.normalized(k)
    }
    /// The AU's display strings for parameters the sweep does not move; the panel's own
    /// fall-backs (step names, percentages) for the swept ones.
    fn display(&self, k: u16) -> Option<String> {
        self.strings.get(&k).cloned()
    }
    fn steps(&self, k: u16) -> Vec<String> {
        self.inner.steps(k)
    }
    fn begin_gesture(&mut self) {}
    fn set(&mut self, k: u16, v: f64) {
        self.inner.set(k, v)
    }
    fn end_gesture(&mut self) {}
}

fn live_state(root: &Path, stem: &str) -> Vec<(u16, f64, String)> {
    let Ok(text) = std::fs::read_to_string(root.join("params").join(format!("{stem}.json"))) else { return vec![] };
    let list: Vec<serde_json::Value> = serde_json::from_str(&text).unwrap_or_default();
    list.iter()
        .filter_map(|p| {
            let (id, v) = (p["id"].as_u64()? as u16, p["value"].as_f64()?);
            let (lo, hi) = (p["min"].as_f64().unwrap_or(0.0), p["max"].as_f64().unwrap_or(1.0));
            Some((
                id,
                if hi > lo { (v - lo) / (hi - lo) } else { v },
                p["display"].as_str().unwrap_or_default().to_string(),
            ))
        })
        .collect()
}

/// Share of pixels (common area) whose largest channel differs by more than 24/255.
fn off(a: &ImageData, b: &ImageData) -> f64 {
    let (w, h) = (a.width.min(b.width), a.height.min(b.height));
    let px = |d: &ImageData, x: u32, y: u32| {
        let i = ((y * d.width + x) * 4) as usize;
        let al = d.rgba[i + 3] as f32 / 255.0;
        [d.rgba[i] as f32 * al, d.rgba[i + 1] as f32 * al, d.rgba[i + 2] as f32 * al]
    };
    let n = (w * h).max(1) as f64;
    let bad = (0..h)
        .flat_map(|y| (0..w).map(move |x| (x, y)))
        .filter(|&(x, y)| px(a, x, y).iter().zip(px(b, x, y)).any(|(p, q)| (p - q).abs() > 24.0))
        .count();
    bad as f64 * 100.0 / n
}

fn main() {
    let filter = std::env::args()
        .skip(1)
        .find(|a| !a.starts_with("--") && Some(a.clone()) != arg("--every") && Some(a.clone()) != arg("--out"));
    let every: usize = arg("--every").and_then(|v| v.parse().ok()).unwrap_or(1).max(1);
    let out = arg("--out").map(PathBuf::from);
    let f_root = std::env::var("PLUGIN_UI_LIVE_REFS").map(PathBuf::from).ok();
    let Some(sweeps) = std::env::var("PLUGIN_UI_SWEEPS")
        .map(PathBuf::from)
        .ok()
        .or_else(|| f_root.as_ref().map(|f| f.join("r_renders")))
    else {
        eprintln!("sweeps: set PLUGIN_UI_LIVE_REFS (or PLUGIN_UI_SWEEPS) to the live reference shots");
        std::process::exit(2);
    };
    let Some(lib) = PluginSpecLibrary::locate() else {
        eprintln!("sweeps: no spec folder");
        std::process::exit(2);
    };
    let resources =
        Resources::locate().unwrap_or(Resources { root: PathBuf::from("/nonexistent"), console_version: None });
    let assets = Assets::load(resources.clone()).unwrap_or_else(|_| {
        Assets::from_parts(resources.clone(), Appearance::default(), Arc::new(ImageCache::new(resources)))
    });
    lib.attach(&assets.images);
    let Some(gpu) = gfx::Headless::new() else {
        eprintln!("sweeps: no GPU adapter");
        std::process::exit(3);
    };
    let mut renderer =
        gfx::Renderer::new(&gpu.device, &gpu.queue, gfx::TextureFormat::Rgba8Unorm, assets.images.clone());
    let tmp = std::env::temp_dir().join(format!("plugin-ui-sweeps-{}.png", std::process::id()));
    let mut dirs: Vec<PathBuf> = std::fs::read_dir(&sweeps)
        .map(|d| d.flatten().map(|e| e.path()).filter(|p| p.join("index.json").is_file()).collect())
        .unwrap_or_default();
    dirs.sort();
    let mut all = Vec::new();
    for dir in dirs {
        let name = dir.file_name().and_then(|n| n.to_str()).unwrap_or_default().to_string();
        if filter.as_ref().is_some_and(|f| !name.contains(f.as_str())) {
            continue;
        }
        let index: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(dir.join("index.json")).unwrap()).unwrap();
        let req = index.get("request").unwrap_or(&index);
        let Some(capture) = req["capture"].as_str() else { continue };
        let region: Vec<f32> = req
            .get("region")
            .or_else(|| index.get("region_pt"))
            .and_then(|r| r.as_array())
            .map(|r| r.iter().filter_map(|v| v.as_f64()).map(|v| v as f32).collect())
            .unwrap_or_default();
        let (Some(entry), [rx, ry, rw, rh]) = (lib.entry_named(capture), region.as_slice()) else { continue };
        let spec = lib.spec(entry).unwrap();
        let live = f_root.as_deref().map(|f| live_state(f, entry.stem())).unwrap_or_default();
        let shots: Vec<&serde_json::Value> = index["shots"]
            .as_array()
            .map(|s| s.iter().filter(|s| s.get("audio").is_none()).step_by(every).collect())
            .unwrap_or_default();
        if shots.is_empty() {
            continue;
        }
        let mut scores = Vec::new();
        let mut worst: Option<(f64, PathBuf, PathBuf)> = None;
        for shot in shots {
            let Some(file) = shot["file"].as_str() else { continue };
            let Some(theirs) = ImageData::decode_png(&dir.join(file)) else { continue };
            let swept: HashMap<u16, f64> = shot["params"]
                .as_object()
                .map(|m| m.iter().filter_map(|(k, v)| Some((k.parse().ok()?, v.as_f64()?))).collect())
                .unwrap_or_default();
            let mut mock = MockParams::from_spec(&spec);
            let mut strings = HashMap::new();
            for (k, v, d) in &live {
                if let Some(p) = mock.params.get_mut(*k as usize) {
                    p.value = *v;
                }
                if !swept.contains_key(k) {
                    strings.insert(*k, d.clone());
                }
            }
            for (k, v) in &swept {
                if let Some(p) = mock.params.get_mut(*k as usize) {
                    p.value = *v;
                }
            }
            let params = Strings { inner: mock, strings };
            let mut panel = PluginPanel::new(spec.clone(), &lib, "/devices/0/inputs/0/effects/0");
            let floats = spec.poll_floats();
            if !floats.is_empty() {
                panel.ingest_floats(&vec!["0"; floats.len()].join(","), &params);
            }
            if let Some((_, p0, n)) = spec.opcode_range() {
                panel.ingest_opcode(p0, &vec![0.0; n.max(0) as usize], &params);
            }
            panel.meters.tick(&spec, 10.0);
            let mut ui = Ui::new(&assets);
            let (sw, sh) = panel.size();
            panel.build(&mut ui, Rect::new(-rx, -ry, sw, sh), &params);
            gpu.render_png(&mut renderer, &ui.list, (*rw, *rh), 2.0, &tmp).expect("render");
            let Some(ours) = ImageData::decode_png(&tmp) else { continue };
            let score = off(&ours, &theirs);
            if let Some(dir_out) = &out {
                if worst.as_ref().is_none_or(|w| score > w.0) {
                    let ours_path = dir_out.join(format!("{name}.ours.png"));
                    std::fs::create_dir_all(dir_out).ok();
                    std::fs::copy(&tmp, &ours_path).ok();
                    worst = Some((score, ours_path, dir.join(file)));
                }
            }
            scores.push(score);
        }
        if scores.is_empty() {
            continue;
        }
        scores.sort_by(f64::total_cmp);
        let median = scores[scores.len() / 2];
        let max = scores[scores.len() - 1];
        println!("{name:40} {:4} shots  median {median:6.2}%  worst {max:6.2}%", scores.len());
        if let Some((_, _, theirs)) = worst {
            println!("    worst shot: {}", theirs.display());
        }
        all.extend(scores);
    }
    let _ = std::fs::remove_file(&tmp);
    if !all.is_empty() {
        all.sort_by(f64::total_cmp);
        println!(
            "all: {} shots, median {:.2}%, 90th percentile {:.2}%",
            all.len(),
            all[all.len() / 2],
            all[all.len() * 9 / 10]
        );
    }
}
