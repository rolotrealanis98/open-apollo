//! Meter drawing without UA assets: a fixture Appearance with the meter skins' geometry (the
//! art is never loaded, only the draw list is inspected).

use houston::Resources;
use rack::model::MeterFrame;
use rack::{Assets, Draw, Rect, Ui};

const FIXTURE: &str = r#"{
    "file_format": "appearance",
    "meters": {
        "Mono-Input-Meter": {
            "width": 8, "height": 196, "peak-height": 1,
            "bounds-content": "0,14,16,372", "bounds-clip": "0,4,16,10",
            "image-background": "meters/bg.png", "image-content": "meters/on.png", "image-clip": "meters/clip.png"
        },
        "Left-Input-Meter": { "parent": "Mono-Input-Meter", "width": 4, "bounds-content": "0,14,8,372" },
        "Right-Input-Meter": { "parent": "Mono-Input-Meter", "width": 4, "bounds-content": "0,14,8,372" },
        "Main-Meter-Bridge": {
            "width": 12, "height": 33, "bounds-content": "2,8,20,56", "bounds-clip": "2,2,20,4",
            "image-background": "meters/bg.png", "image-content": "meters/on.png", "image-clip": "meters/clip.png"
        }
    }
}"#;

fn assets() -> Assets {
    static DIR: std::sync::OnceLock<std::path::PathBuf> = std::sync::OnceLock::new();
    let dir = DIR.get_or_init(|| {
        let dir = std::env::temp_dir().join(format!("rack-meters-fixture-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("appearance")).unwrap();
        std::fs::write(dir.join("appearance/Appearance.json"), FIXTURE).unwrap();
        dir
    });
    Assets::load(Resources::at(dir).expect("fixture resources")).expect("fixture appearance")
}

/// The one-point clip rects drawn for the peak line, in draw order.
fn peak_lines(ui: &Ui) -> Vec<Rect> {
    ui.list.items.iter().filter_map(|d| match d {
        Draw::Clip(Some(r)) if r.h == 1.0 => Some(*r),
        _ => None,
    }).collect()
}

#[test]
fn a_peak_draws_a_one_point_line_at_its_height() {
    let assets = assets();
    let r = Rect::new(0.0, 0.0, 8.0, 200.0);
    // Well: 14 art px (7 pt) of cap above, 392 − 14 − 372 = 6 art px (3 pt) below.
    let mut ui = Ui::new(&assets);
    ui.bar_meter("Mono-Input-Meter", 0.25, 0.75, false, r);
    let lines = peak_lines(&ui);
    assert_eq!(lines.len(), 1, "one peak line");
    // The line's foot sits at 75 % of the travel from the well's bottom.
    let (top, bottom) = (7.0, 200.0 - 3.0);
    let want = bottom - (bottom - top) * 0.75;
    assert!((lines[0].max_y() - want).abs() < 0.01, "{:?} vs {want}", lines[0]);
    // Silent peak, or a skin without `peak-height` (the Meter Bridge): no line.
    let mut ui = Ui::new(&assets);
    ui.bar_meter("Mono-Input-Meter", 0.25, 0.0, false, r);
    ui.bar_meter("Main-Meter-Bridge", 0.25, 0.75, false, r);
    assert!(peak_lines(&ui).is_empty());
}

#[test]
fn the_peak_line_stays_inside_the_well_at_full_scale() {
    let assets = assets();
    let r = Rect::new(0.0, 0.0, 8.0, 200.0);
    let mut ui = Ui::new(&assets);
    ui.bar_meter("Mono-Input-Meter", 1.0, 1.0, false, r);
    let line = peak_lines(&ui)[0];
    assert!(line.y >= 7.0 - 0.01 && line.max_y() <= 197.0 + 0.01, "{line:?}");
}

#[test]
fn fader_and_meters_draws_a_peak_per_channel() {
    let assets = assets();
    let frame = MeterFrame { left: 0.2, right: 0.1, peak_left: 0.6, peak_right: 0.4, ..Default::default() };
    let mut ui = Ui::new(&assets);
    ui.fader_and_meters("Fader", true, 0.5, frame, "0.0", Rect::new(0.0, 0.0, 96.0, 300.0), None);
    let lines = peak_lines(&ui);
    assert_eq!(lines.len(), 2, "left and right peaks");
    assert!(lines[0].max_y() < lines[1].max_y(), "the higher peak sits higher: {lines:?}");
}
