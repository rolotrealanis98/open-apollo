//! Headless renders of the window in several `ViewState`s (navigator toggles, browser filters,
//! pages, popovers, sheet), for checking the UI surfaces.
//!
//!   cargo run --release -p rack --example surfaces -- [out_dir] [name…]

use std::path::PathBuf;

use houston::Resources;
use rack::geometry::StripKind;
use rack::model::{InsertState, MainState, StripInfo};
use rack::surfaces::{InsertSize, Modifier, MonitorPage, Popover, PopoverState, Section, Sheet, TextEdit, TextTarget};
use rack::{Assets, ConsoleModel, Control, ControlId, MixerModel, PreviewMixer, Ui, ViewState, build_window_with};

fn id(strip: &str, control: Control) -> ControlId {
    ControlId { strip: strip.into(), control }
}

fn popover(kind: Popover, anchor: ControlId) -> Option<PopoverState> {
    Some(PopoverState { kind, anchor })
}

/// PreviewMixer with 8 extra ADAT inputs, so the strips scroll.
fn many_inputs() -> PreviewMixer {
    let mut m = PreviewMixer::default();
    for i in 0..8 {
        let sid = format!("adat{i}");
        let s = StripInfo { id: sid.as_str().into(), title: format!("ADAT {}", i + 1), kind: StripKind::Input, stereo: false, preamp_layout: None, shows_sr_convert: false, console_model: None };
        m.mains.insert(s.id.clone(), MainState { name: format!("Track {}", i + 7), fader: 0.4 + 0.05 * i as f64, solo: i == 3, ..Default::default() });
        m.inserts.insert(s.id.clone(), vec![InsertState::Empty; 4]);
        m.inputs.push(s);
    }
    m
}

/// PreviewMixer with every strip assigned `model` (console-model strip skins).
fn with_model(model: ConsoleModel) -> PreviewMixer {
    let mut m = PreviewMixer::default();
    for s in m.inputs.iter_mut().chain(m.auxes.iter_mut()) {
        s.console_model = Some(model);
    }
    m
}

fn scenarios() -> Vec<(&'static str, Box<dyn Fn() -> (PreviewMixer, ViewState)>)> {
    let base = || (PreviewMixer::default(), ViewState::default());
    vec![
        ("full", Box::new(base)),
        ("model-api-summing", Box::new(|| (with_model(ConsoleModel::ApiSumming), ViewState::default()))),
        ("model-neve-8028", Box::new(|| (with_model(ConsoleModel::Neve8028), ViewState::default()))),
        (
            "collapsed",
            Box::new(|| {
                let mut v = ViewState::default();
                v.sections.inputs = false;
                v.insert_size = InsertSize::Small;
                v.fixed_slots = false;
                v.modifier = Some(Modifier::Power);
                (PreviewMixer::default(), v)
            }),
        ),
        (
            "scrolled-sends",
            Box::new(|| {
                let v = ViewState { cells_scroll: 1.0, ..ViewState::default() };
                (PreviewMixer::default(), v)
            }),
        ),
        (
            "overview-nofader",
            Box::new(|| {
                let mut v = ViewState { sends_overview: true, ..ViewState::default() };
                v.sections.fader = false;
                v.sections.inputs = false;
                (PreviewMixer::default(), v)
            }),
        ),
        (
            "closed",
            Box::new(|| {
                let mut v = ViewState::default();
                for s in [Section::Inputs, Section::Inserts, Section::Sends] {
                    v.sections.set(s, false);
                }
                (PreviewMixer::default(), v)
            }),
        ),
        (
            "ctrlroom-cues",
            Box::new(|| {
                let v = ViewState {
                    show_browser: false,
                    monitor_page: MonitorPage::ControlRoom,
                    popover: popover(Popover::CueOutputs, id("monitor", Control::CueOutputs)),
                    ..ViewState::default()
                };
                (PreviewMixer::default(), v)
            }),
        ),
        (
            "browser-filter",
            Box::new(|| {
                let mut v = ViewState::default();
                v.browser.category = Some(1);
                v.browser.hint = false;
                v.browser.selected = Some(27);
                v.text_edit = Some(TextEdit { target: TextTarget::Search, text: "a".into() });
                v.browser.search = "a".into();
                (PreviewMixer::default(), v)
            }),
        ),
        (
            "browser-scrolled",
            Box::new(|| {
                let mut v = ViewState::default();
                v.browser.categories_open = false;
                v.browser.large_rows = false;
                v.browser.scroll = 0.6;
                (PreviewMixer::default(), v)
            }),
        ),
        (
            "popover-source",
            Box::new(|| {
                let v = ViewState { popover: popover(Popover::InputSource { strip: "in1".into() }, id("in1", Control::Preamp("input_source".into()))), ..ViewState::default() };
                (PreviewMixer::default(), v)
            }),
        ),
        (
            "popover-insert",
            Box::new(|| {
                let v = ViewState { popover: popover(Popover::InsertMenu { strip: "in2".into(), slot: 1 }, id("in2", Control::Insert(1))), ..ViewState::default() };
                (PreviewMixer::default(), v)
            }),
        ),
        (
            "popover-rename",
            Box::new(|| {
                let v = ViewState {
                    popover: popover(Popover::Rename { strip: "in1".into() }, id("in1", Control::Name)),
                    text_edit: Some(TextEdit { target: TextTarget::Rename("in1".into()), text: "GTR Amp".into() }),
                    ..ViewState::default()
                };
                (PreviewMixer::default(), v)
            }),
        ),
        (
            "popover-scenes",
            Box::new(|| {
                let v = ViewState { popover: popover(Popover::Scenes, id("nav", Control::ShowPluginScenes)), ..ViewState::default() };
                (PreviewMixer::default(), v)
            }),
        ),
        (
            "popover-rate",
            Box::new(|| {
                let v = ViewState { popover: popover(Popover::SampleRate, id("status", Control::StatusRate)), ..ViewState::default() };
                (PreviewMixer::default(), v)
            }),
        ),
        (
            "settings",
            Box::new(|| {
                let v = ViewState { sheet: Some(Sheet::Settings), popover: popover(Popover::ClockSource, id("settings", Control::SettingsClock)), ..ViewState::default() };
                (PreviewMixer::default(), v)
            }),
        ),
        (
            "many-strips",
            Box::new(|| {
                let v = ViewState { first_strip: 3, ..ViewState::default() };
                (many_inputs(), v)
            }),
        ),
        ("compact", Box::new(|| (PreviewMixer::default(), ViewState::compact()))),
    ]
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let out = PathBuf::from(args.first().cloned().unwrap_or_else(|| "shots".into()));
    let only: Vec<&String> = args.iter().skip(1).collect();
    std::fs::create_dir_all(&out).expect("out dir");
    let Some(resources) = Resources::locate() else {
        eprintln!("surfaces: UAD Console resources not found");
        std::process::exit(2);
    };
    let assets = Assets::load(resources).expect("appearance");
    let Some(gpu) = gfx::Headless::new() else {
        eprintln!("surfaces: no GPU adapter");
        std::process::exit(3);
    };
    let mut renderer = gfx::Renderer::new(&gpu.device, &gpu.queue, gfx::TextureFormat::Rgba8Unorm, assets.images.clone());
    for (name, make) in scenarios() {
        if !only.is_empty() && !only.iter().any(|o| o.as_str() == name) {
            continue;
        }
        let (model, view) = make();
        let mut ui = Ui::new(&assets);
        let layout = build_window_with(&mut ui, &model as &dyn MixerModel, &view);
        let path = out.join(format!("surfaces-{name}.png"));
        gpu.render_png(&mut renderer, &ui.list, layout.size(), 2.0, &path).expect("png");
        eprintln!("{}: {} draws, {} hits, {} scroll regions", path.display(), ui.list.len(), ui.hits.len(), ui.scrolls.len());
    }
    let missing = assets.images.missing();
    if !missing.is_empty() {
        eprintln!("missing images: {missing:?}");
    }
}
