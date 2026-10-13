//! Which plug-in panels can be drawn from this install, and why the others show the generic list.
//!
//!   cargo run -p plugin-ui --example availability -- [--specs DIR] [--art DIR] [--all]
//!
//! `--specs DIR` reads that spec folder (default: [`SpecLocator`]); `--art DIR` the generated art
//! root (default: [`ArtLocator`]). Prints one line per panel that is not ready (`--all`: every
//! panel) and a total; exits 1 when a spec fails to load.

use std::path::PathBuf;

use plugin_ui::{ArtLocator, Availability, PluginSpecLibrary, SpecLocator};

fn arg(name: &str) -> Option<String> {
    let args: Vec<String> = std::env::args().collect();
    args.iter().position(|a| a == name).and_then(|i| args.get(i + 1)).cloned()
}

fn main() {
    let all = std::env::args().any(|a| a == "--all");
    let Some(specs) = arg("--specs").map(PathBuf::from).or_else(SpecLocator::locate) else {
        eprintln!("no plug-in spec folder (pass --specs DIR or set {})", SpecLocator::ENV);
        std::process::exit(1);
    };
    let art = arg("--art").map(PathBuf::from).unwrap_or_else(ArtLocator::locate);
    let lib = match PluginSpecLibrary::open_with_art(&specs, &art) {
        Ok(lib) => lib,
        Err(e) => {
            eprintln!("{e}");
            std::process::exit(1);
        }
    };
    let (mut ready, mut pending, mut unavailable, mut failed) = (0, 0, 0, 0);
    for entry in lib.entries.clone() {
        let stem = entry.stem().to_string();
        let spec = match lib.spec(&entry) {
            Ok(spec) => spec,
            Err(e) => {
                println!("{stem}: does not load: {e}");
                failed += 1;
                continue;
            }
        };
        let line = match lib.availability_of(&stem, &spec) {
            Availability::Ready => {
                ready += 1;
                all.then(|| format!("{stem}: ready"))
            }
            Availability::Pending(ids) => {
                pending += 1;
                Some(format!(
                    "{stem}: pending ({} ids: {})",
                    ids.len(),
                    ids.iter().take(4).cloned().collect::<Vec<_>>().join(", ")
                ))
            }
            Availability::Unavailable(ids) => {
                unavailable += 1;
                Some(format!(
                    "{stem}: unavailable ({} ids: {})",
                    ids.len(),
                    ids.iter().take(4).cloned().collect::<Vec<_>>().join(", ")
                ))
            }
        };
        if let Some(line) = line {
            println!("{line}");
        }
    }
    println!(
        "{} panels: {ready} ready, {pending} pending, {unavailable} unavailable, {failed} failed to load ({} with art {})",
        lib.entries.len(),
        specs.display(),
        art.display()
    );
    if failed > 0 {
        std::process::exit(1);
    }
}
