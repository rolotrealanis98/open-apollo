//! uaconsole — a native UAD Console for open-apollo. See `cli::USAGE`.

mod app;
mod bench;
mod cli;
mod frame;
mod input;
mod overlay;
mod plugin;
mod popups;
mod settings;
mod source;
mod view;

use std::process::ExitCode;
use std::time::Instant;

use houston::Resources;
use rack::Assets;

use cli::Args;
use settings::Settings;
use source::ModelSource;
use view::View;

fn main() -> ExitCode {
    let args = match Args::parse(std::env::args().skip(1)) {
        Ok(a) => a,
        Err(e) => {
            eprintln!("uaconsole: {e}\n\n{}", cli::USAGE);
            return ExitCode::from(2);
        }
    };
    if args.help {
        println!("{}", cli::USAGE);
        return ExitCode::SUCCESS;
    }
    let Some(assets) = load_assets() else {
        return ExitCode::from(2);
    };
    let headless = args.render.is_some() || args.bench.is_some();
    let settings = Settings::load();

    // The window connects to the local engine unless told otherwise; headless modes use
    // preview data unless --engine/--mock is given.
    let engine = args
        .engine
        .clone()
        .or_else(|| (!headless).then(cli::EngineAddr::local));
    let source = match (&args.mock, engine) {
        _ if args.preview => ModelSource::Preview,
        (Some(path), _) => ModelSource::Mock(path.clone()),
        (None, Some(addr)) => ModelSource::Engine {
            addr,
            writes: args.writes,
        },
        (None, None) => ModelSource::Preview,
    };
    if args.writes && !matches!(source, ModelSource::Engine { .. }) {
        eprintln!("uaconsole: --writes has no effect without an engine");
    }

    if headless {
        let mut opened = source.open().unwrap_or_else(|e| {
            eprintln!("uaconsole: {e}; showing preview data");
            source::preview("preview")
        });
        if matches!(source, ModelSource::Engine { .. }) {
            // Headless frames from a live engine: wait (≤10 s) for the tree and first pushes.
            let deadline = Instant::now() + std::time::Duration::from_secs(10);
            while Instant::now() < deadline {
                opened.model.pump();
                if !opened.model.inputs().is_empty() {
                    std::thread::sleep(std::time::Duration::from_millis(500));
                    opened.model.pump();
                    break;
                }
                std::thread::sleep(std::time::Duration::from_millis(50));
            }
            eprintln!("engine: {} inputs, {} auxes", opened.model.inputs().len(), opened.model.auxes().len());
        }
        let scale = args.scale.unwrap_or(2.0);
        let result = match (&args.render, args.bench) {
            (Some(out), _) => bench::render_png(
                &assets,
                &*opened.model,
                args.compact,
                args.size,
                scale,
                args.open_plugin.as_ref(),
                args.open.as_deref(),
                args.view_spec.as_deref(),
                args.menu.as_deref(),
                args.menu_bar,
                args.screen,
                &args.window_pos,
                out,
            ),
            (None, Some(n)) => {
                bench::bench(&assets, &mut *opened.model, args.compact, scale, n.max(1))
            }
            (None, None) => unreachable!("headless without --render/--bench"),
        };
        return match result {
            Ok(()) => ExitCode::SUCCESS,
            Err(e) => {
                eprintln!("uaconsole: {e}");
                ExitCode::from(3)
            }
        };
    }

    let mode = args.view.unwrap_or(settings.view);
    let opts = app::Options {
        source,
        view: View::new(mode, settings.zoom),
        compact: args.compact || settings.compact,
        live_meters: args.live_meters,
        stats: args.stats,
        open_plugin: args.open_plugin,
        open: args.open,
        view_spec: args.view_spec,
        menu: args.menu,
        windows: args.windows.unwrap_or(cli::WindowsMode::Separate),
        x11: args.x11,
        settings,
    };
    match app::run(assets, opts) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("uaconsole: {e}");
            ExitCode::from(1)
        }
    }
}

fn load_assets() -> Option<Assets> {
    let Some(resources) = Resources::locate() else {
        eprintln!("uaconsole: UAD Console resources not found. Looked in:");
        for c in Resources::candidates() {
            eprintln!("  {}", c.display());
        }
        return None;
    };
    let t = Instant::now();
    let assets = Assets::load(resources)
        .map_err(|e| eprintln!("uaconsole: appearance: {e}"))
        .ok()?;
    eprintln!(
        "resources: {} (Console {}), {} skins in {:.1} ms",
        assets.resources.root.display(),
        assets.resources.console_version.as_deref().unwrap_or("?"),
        assets.appearance.skins.len(),
        t.elapsed().as_secs_f64() * 1e3
    );
    Some(assets)
}
