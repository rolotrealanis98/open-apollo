//! Copying assets out of the user's install. The source is never written to.

use std::io;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::{Value, json};

use crate::detect::Console;

/// Folder name under the data dir; houston's `Resources::candidates` looks here.
pub const CONSOLE_DIR: &str = "console-resources";
pub const MANIFEST: &str = "manifest.json";

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Copied {
    pub dest: PathBuf,
    pub files: u64,
    pub bytes: u64,
}

/// Copy the Console `Resources` folder to `<data>/console-resources`.
///
/// Copies into a sibling `.partial` folder first and swaps it in at the end, so
/// an interrupted import leaves the previous one intact.
pub fn import_console(console: &Console, data: &Path) -> io::Result<Copied> {
    std::fs::create_dir_all(data)?;
    let dest = data.join(CONSOLE_DIR);
    let partial = data.join(format!(".{CONSOLE_DIR}.partial"));
    if partial.exists() {
        std::fs::remove_dir_all(&partial)?;
    }
    let (files, bytes) = copy_tree(&console.resources, &partial)?;
    if dest.exists() {
        std::fs::remove_dir_all(&dest)?;
    }
    std::fs::rename(&partial, &dest)?;
    record(
        data,
        "console",
        json!({
            "from": console.resources,
            "version": console.version,
            "files": files,
            "bytes": bytes,
            "imported_at": unix_now(),
        }),
    )?;
    Ok(Copied { dest, files, bytes })
}

/// Count what `import_console` would copy.
pub fn measure(dir: &Path) -> io::Result<(u64, u64)> {
    let mut files = 0;
    let mut bytes = 0;
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        let ty = entry.file_type()?;
        if ty.is_dir() {
            let (f, b) = measure(&entry.path())?;
            files += f;
            bytes += b;
        } else if ty.is_file() {
            files += 1;
            bytes += entry.metadata()?.len();
        }
    }
    Ok((files, bytes))
}

fn copy_tree(from: &Path, to: &Path) -> io::Result<(u64, u64)> {
    std::fs::create_dir_all(to)?;
    let mut files = 0;
    let mut bytes = 0;
    for entry in std::fs::read_dir(from)? {
        let entry = entry?;
        let ty = entry.file_type()?;
        let target = to.join(entry.file_name());
        if ty.is_dir() {
            let (f, b) = copy_tree(&entry.path(), &target)?;
            files += f;
            bytes += b;
        } else if ty.is_file() {
            bytes += std::fs::copy(entry.path(), &target)?;
            files += 1;
        }
    }
    Ok((files, bytes))
}

/// Read `<data>/manifest.json`, or an empty object.
pub fn read_manifest(data: &Path) -> Value {
    std::fs::read(data.join(MANIFEST))
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok())
        .filter(Value::is_object)
        .unwrap_or_else(|| json!({}))
}

fn record(data: &Path, key: &str, value: Value) -> io::Result<()> {
    let mut m = read_manifest(data);
    m["schema"] = json!(1);
    m[key] = value;
    let text = serde_json::to_string_pretty(&m).map_err(io::Error::other)?;
    std::fs::write(data.join(MANIFEST), text + "\n")
}

fn unix_now() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn copies_resources_and_replaces_previous_import() {
        let t = std::env::temp_dir().join(format!("oa-assets-import-{}", std::process::id()));
        let _ = fs::remove_dir_all(&t);
        let res = t.join("src/Resources");
        fs::create_dir_all(res.join("appearance")).unwrap();
        fs::write(res.join("appearance/Appearance.json"), "{}").unwrap();
        fs::create_dir_all(res.join("images")).unwrap();
        fs::write(res.join("images/a.png"), [1u8; 10]).unwrap();
        let console = Console { dir: t.join("src"), resources: res, version: Some("1.2.9.905".into()) };
        let data = t.join("data");
        fs::create_dir_all(data.join(CONSOLE_DIR)).unwrap();
        fs::write(data.join(CONSOLE_DIR).join("stale.txt"), "old").unwrap();

        let got = import_console(&console, &data).unwrap();
        assert_eq!((got.files, got.bytes), (2, 12));
        assert!(data.join(CONSOLE_DIR).join("appearance/Appearance.json").is_file());
        assert!(!data.join(CONSOLE_DIR).join("stale.txt").exists());
        assert!(!data.join(format!(".{CONSOLE_DIR}.partial")).exists());
        let m = read_manifest(&data);
        assert_eq!(m["console"]["version"], "1.2.9.905");
        assert_eq!(m["console"]["files"], 2);
        fs::remove_dir_all(&t).unwrap();
    }
}
