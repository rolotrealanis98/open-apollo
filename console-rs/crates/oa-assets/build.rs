//! Embeds `data/plugin-recipes/*.json`, deflated, so the binary carries its recipes.

use std::fmt::Write;
use std::io::Write as _;
use std::path::Path;

fn main() {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("data/plugin-recipes");
    println!("cargo:rerun-if-changed={}", dir.display());
    let mut names: Vec<_> = std::fs::read_dir(&dir)
        .map(|rd| rd.flatten().map(|e| e.path()).filter(|p| p.extension().is_some_and(|x| x == "json")).collect())
        .unwrap_or_default();
    names.sort();
    let out_dir = std::env::var("OUT_DIR").expect("OUT_DIR");
    let mut src =
        String::from("/// Built-in recipe files (name, deflated JSON).\npub static RECIPES: &[(&str, &[u8])] = &[\n");
    for p in &names {
        println!("cargo:rerun-if-changed={}", p.display());
        let stem = p.file_stem().unwrap_or_default().to_string_lossy().to_string();
        let text = std::fs::read(p).expect("read recipe file");
        let mut z = flate2::write::DeflateEncoder::new(Vec::new(), flate2::Compression::best());
        z.write_all(&text).expect("deflate");
        let packed = Path::new(&out_dir).join(format!("{stem}.json.deflate"));
        std::fs::write(&packed, z.finish().expect("deflate")).expect("write packed recipe");
        let _ = writeln!(src, "    ({stem:?}, include_bytes!({:?})),", packed.display().to_string());
    }
    src.push_str("];\n");
    std::fs::write(Path::new(&out_dir).join("recipes_index.rs"), src).expect("write recipes index");
}
