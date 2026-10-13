//! Embeds `data/plugin-recipes/*.json` so the binary carries its recipes.

use std::fmt::Write;
use std::path::Path;

fn main() {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("data/plugin-recipes");
    println!("cargo:rerun-if-changed={}", dir.display());
    let mut names: Vec<_> = std::fs::read_dir(&dir)
        .map(|rd| rd.flatten().map(|e| e.path()).filter(|p| p.extension().is_some_and(|x| x == "json")).collect())
        .unwrap_or_default();
    names.sort();
    let mut src = String::from("/// Built-in recipe files (name, JSON).\npub static RECIPES: &[(&str, &str)] = &[\n");
    for p in &names {
        println!("cargo:rerun-if-changed={}", p.display());
        let stem = p.file_stem().unwrap_or_default().to_string_lossy();
        let _ = writeln!(src, "    ({stem:?}, include_str!({:?})),", p.display().to_string());
    }
    src.push_str("];\n");
    let out = Path::new(&std::env::var("OUT_DIR").expect("OUT_DIR")).join("recipes_index.rs");
    std::fs::write(out, src).expect("write recipes index");
}
