//! Console-model strip skins: synthetic strips with each `ConsoleModel` draw the model's fader,
//! send plate, send knob and bus bottom, and the default (no model) keeps Console's defaults.
//! A fixture Appearance gives every key a distinct image or text colour, so the draw list shows
//! which key was used. No UA assets.

use std::path::PathBuf;

use houston::{Resources, Rgba};
use rack::model::StripInfo;
use rack::{Assets, ConsoleModel, Draw, PreviewMixer, Ui, ViewState, build_window_with};

const FIXTURE: &str = r#"{
    "file_format": "appearance",
    "faders": {
        "Main-Input-Fader": { "color-text": "0,0,0,1" },
        "Main-Bus-Fader": { "color-text": "200,200,200,1" },
        "Main-Input-Fader-API-Summing": { "color-text": "10,20,30,1" },
        "Main-Input-Fader-Neve-8028": { "color-text": "40,50,60,1" }
    },
    "backgrounds": {
        "Send-Lower-State-Plate": { "image": "t/plate.png" },
        "Lower-State-Plate-API-Summing": { "image": "t/plate_api.png" },
        "Lower-State-Plate-Neve-8028": { "image": "t/plate_neve.png" },
        "Aux-Channel-Bottom-API-Summing": { "image": "t/bottom_api.png" },
        "Aux-Channel-Bottom-Neve-8028": { "image": "t/bottom_neve.png" }
    },
    "knobs": {
        "Mini-Send-Level": { "image-film": "t/send.png", "frame-height": 2 },
        "Gain-API-Summing": { "image-film": "t/gain_api.png", "frame-height": 2 },
        "Gain-Neve-8028": { "image-film": "t/gain_neve.png", "frame-height": 2 },
        "Pan-Black": { "image-film": "t/pan_black.png", "frame-height": 2 },
        "Pan-White": { "image-film": "t/pan_white.png", "frame-height": 2 },
        "Pan-API-Summing": { "image-film": "t/pan_api.png", "frame-height": 2 },
        "Pan-Neve-8028": { "image-film": "t/pan_neve.png", "frame-height": 2 }
    }
}"#;

/// A 2×4 RGBA PNG (two 2-pt frames).
const PNG: &[u8] = &[
    137, 80, 78, 71, 13, 10, 26, 10, 0, 0, 0, 13, 73, 72, 68, 82, 0, 0, 0, 2, 0, 0, 0, 4, 8, 6, 0, 0, 0, 164, 239, 238, 57, 0, 0, 0, 17, 73, 68, 65, 84, 120,
    156, 99, 248, 207, 192, 240, 31, 132, 25, 112, 51, 0, 31, 19, 15, 241, 191, 72, 108, 201, 0, 0, 0, 0, 73, 69, 78, 68, 174, 66, 96, 130,
];

const IMAGES: [&str; 12] = [
    "plate", "plate_api", "plate_neve", "bottom_api", "bottom_neve", "send", "gain_api", "gain_neve", "pan_black", "pan_white", "pan_api", "pan_neve",
];

fn assets() -> Assets {
    static DIR: std::sync::OnceLock<PathBuf> = std::sync::OnceLock::new();
    let dir = DIR.get_or_init(|| {
        let dir = std::env::temp_dir().join(format!("rack-console-models-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("appearance")).unwrap();
        std::fs::create_dir_all(dir.join("images/t")).unwrap();
        std::fs::write(dir.join("appearance/Appearance.json"), FIXTURE).unwrap();
        for name in IMAGES {
            std::fs::write(dir.join(format!("images/t/{name}.png")), PNG).unwrap();
        }
        dir
    });
    Assets::load(Resources::at(dir).expect("fixture resources")).expect("fixture appearance")
}

/// The preview mixer with every strip assigned `model`.
fn mixer(model: Option<ConsoleModel>) -> PreviewMixer {
    let mut m = PreviewMixer::default();
    for s in m.inputs.iter_mut().chain(m.auxes.iter_mut()) {
        s.console_model = model;
    }
    m
}

struct Drawn {
    images: Vec<String>,
    text_colors: Vec<Rgba>,
}

fn draw(model: Option<ConsoleModel>) -> Drawn {
    let assets = assets();
    let mixer = mixer(model);
    let mut ui = Ui::new(&assets);
    build_window_with(&mut ui, &mixer, &ViewState::default());
    let mut images = Vec::new();
    let mut text_colors = Vec::new();
    for item in &ui.list.items {
        match item {
            Draw::Image { key, .. } => images.push(key.trim_start_matches("t/").trim_end_matches(".png").to_string()),
            Draw::Text { style, .. } => text_colors.push(style.color),
            _ => {}
        }
    }
    Drawn { images, text_colors }
}

fn has(d: &Drawn, image: &str) -> bool {
    count(d, image) > 0
}

fn count(d: &Drawn, image: &str) -> usize {
    d.images.iter().filter(|i| *i == image).count()
}

#[test]
fn strip_skin_keys_follow_the_console_model() {
    let strip = |kind, model| StripInfo { id: "s".into(), title: "S".into(), kind, stereo: true, preamp_layout: None, shows_sr_convert: false, console_model: model };
    use rack::geometry::StripKind::{Aux, Input};
    // Default (no model): Console's keys.
    let (i, a) = (strip(Input, None), strip(Aux, None));
    assert_eq!((i.fader_skin(), a.fader_skin()), ("Main-Input-Fader", "Main-Bus-Fader"));
    assert_eq!((i.pan_skin(), a.pan_skin()), ("Pan-Black", "Pan-White"));
    assert_eq!((i.send_plate_skin(), i.send_knob_skin()), ("Send-Lower-State-Plate", "Mini-Send-Level"));
    assert_eq!(a.aux_bottom_skin(), None);
    for m in ConsoleModel::ALL {
        let sfx = m.skin_suffix();
        assert_eq!(ConsoleModel::from_plugin_id(m.plugin_id()), Some(m));
        let (i, a) = (strip(Input, Some(m)), strip(Aux, Some(m)));
        // UpdateFaderInfoID: the model's fader on inputs and buses alike.
        assert_eq!(i.fader_skin(), format!("Main-Input-Fader-{sfx}"));
        assert_eq!(a.fader_skin(), format!("Main-Input-Fader-{sfx}"));
        // Rack_Pans: inputs stay Pan-Black; buses take the model's Pan-*.
        assert_eq!((i.pan_skin(), a.pan_skin()), ("Pan-Black", format!("Pan-{sfx}").as_str()));
        assert_eq!(m.pan_skin(false), format!("Input-Pan-{sfx}"));
        assert_eq!(a.send_plate_skin(), format!("Lower-State-Plate-{sfx}"));
        assert_eq!(a.send_knob_skin(), format!("Gain-{sfx}"));
        assert_eq!(a.aux_bottom_skin(), Some(format!("Aux-Channel-Bottom-{sfx}").as_str()));
        assert_eq!(i.aux_bottom_skin(), None, "inputs keep Channel-Bottom");
    }
    assert_eq!(ConsoleModel::Neve8028.skin_suffix(), "Neve-8028");
    assert_eq!(ConsoleModel::Api2500.skin_suffix(), "API-Summing");
    assert_eq!(ConsoleModel::from_plugin_id("uaudio_ampex_atr_102_ext"), None, "model 5 has no strip customization");
    assert_eq!(ConsoleModel::from_plugin_id(""), None);
}

#[test]
fn window_draws_each_models_skins() {
    let rgb = |r: u8, g: u8, b: u8| Rgba::rgb8(r, g, b);
    let default = draw(None);
    assert!(has(&default, "plate") && has(&default, "send"), "aux sends use the default plate and knob");
    assert!(has(&default, "pan_black"));
    assert!(default.text_colors.contains(&rgb(0, 0, 0)) && default.text_colors.contains(&rgb(200, 200, 200)));
    for img in ["plate_api", "plate_neve", "bottom_api", "bottom_neve", "gain_api", "gain_neve"] {
        assert!(!has(&default, img), "{img} drawn without a model");
    }
    for m in ConsoleModel::ALL {
        let (fam, color) = if m == ConsoleModel::Neve8028 { ("neve", rgb(40, 50, 60)) } else { ("api", rgb(10, 20, 30)) };
        let d = draw(Some(m));
        for img in ["plate", "gain", "bottom"] {
            assert!(has(&d, &format!("{img}_{fam}")), "{m:?}: no {img}_{fam}");
        }
        // Every send row swaps plate and knob; the preamp plate and the monitor's dim knob are
        // not strip skins and keep the defaults.
        assert_eq!(count(&d, "plate") + count(&d, &format!("plate_{fam}")), count(&default, "plate"), "{m:?}: plates");
        assert_eq!(count(&d, "send") + count(&d, &format!("gain_{fam}")), count(&default, "send"), "{m:?}: send knobs");
        assert!(has(&d, "pan_black"), "{m:?}: input pans stay Pan-Black");
        assert!(d.text_colors.contains(&color), "{m:?}: model fader marks");
        assert!(!d.text_colors.contains(&rgb(200, 200, 200)), "{m:?}: no Main-Bus-Fader left");
    }
}
