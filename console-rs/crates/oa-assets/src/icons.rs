//! `oa-assets icons`: Console's per-plug-in browser icons (`Plug-In Icons/<EffectName>.png`) into
//! `<data>/console-resources/Plug-In Icons/`, where houston's `Resources::plugin_icons_dir` reads them.
//!
//! UAD installs the folder only when it detected an Apollo. Without one, the PNGs are still in the
//! cached `apollo-32bit.msi`: a CFB file whose streams are the embedded cabinets. A cabinet names
//! its members by the MSI File table's key (`UAD_1176.png`), not the real name (`UAD 1176.png`),
//! so the File table is read to restore the names; a table that can't be read leaves the keys.
//! Only PNG members are taken, and the source is never written to.

use std::collections::{BTreeSet, HashMap};
use std::io::{self, Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

use serde_json::json;

use crate::detect::{PluginIcons, has_ext};
use crate::import::{CONSOLE_DIR, record, unix_now};

/// Folder name houston looks for inside the Console resources.
pub const DIR: &str = "Plug-In Icons";
const MAX_PNG: u64 = 4 << 20;
const PNG_MAGIC: &[u8] = b"\x89PNG\r\n\x1a\n";

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Imported {
    pub dest: PathBuf,
    pub count: usize,
}

/// Replace `<data>/console-resources/Plug-In Icons` with the PNGs from `src`, and record the step in
/// `manifest.json` (`plugin_icons`: from, count). Errors if `src` holds no PNG (the old folder stays).
pub fn import(src: &PluginIcons, data: &Path) -> Result<Imported, String> {
    let (PluginIcons::Folder(from) | PluginIcons::CachedMsi(from)) = src;
    let res = data.join(CONSOLE_DIR);
    let (dest, partial) = (res.join(DIR), res.join(format!("{DIR}.partial")));
    let run = || -> io::Result<usize> {
        if partial.exists() {
            std::fs::remove_dir_all(&partial)?;
        }
        std::fs::create_dir_all(&partial)?;
        let n = match src {
            PluginIcons::Folder(dir) => copy_folder(dir, &partial)?,
            PluginIcons::CachedMsi(msi) => extract_msi(msi, &partial)?,
        };
        if n == 0 {
            std::fs::remove_dir_all(&partial)?;
            return Ok(0);
        }
        if dest.exists() {
            std::fs::remove_dir_all(&dest)?;
        }
        std::fs::rename(&partial, &dest)?;
        Ok(n)
    };
    let count = run().map_err(|e| format!("{}: {e}", from.display()))?;
    if count == 0 {
        return Err(format!("no PNG icons in {}", from.display()));
    }
    record(data, "plugin_icons", json!({"from": from, "count": count, "imported_at": unix_now()}))
        .map_err(|e| e.to_string())?;
    Ok(Imported { dest, count })
}

fn copy_folder(from: &Path, to: &Path) -> io::Result<usize> {
    let mut n = 0;
    for e in std::fs::read_dir(from)? {
        let e = e?;
        if e.file_type()?.is_file() && has_ext(&e.path(), "png") {
            std::fs::copy(e.path(), to.join(e.file_name()))?;
            n += 1;
        }
    }
    Ok(n)
}

/// Write every PNG member of every cabinet embedded in the MSI to `out`; the number of distinct names.
fn extract_msi(msi: &Path, out: &Path) -> io::Result<usize> {
    let mut cfb = cfb::open(msi)?;
    let streams: Vec<PathBuf> = cfb.walk().filter(|e| e.is_stream()).map(|e| e.path().to_path_buf()).collect();
    let names = file_table(&mut cfb);
    let mut written = BTreeSet::new();
    for path in streams {
        let mut s = cfb.open_stream(&path)?;
        let mut magic = [0u8; 4];
        if s.read_exact(&mut magic).is_err() || &magic != b"MSCF" {
            continue;
        }
        s.seek(SeekFrom::Start(0))?;
        let Ok(mut cab) = cab::Cabinet::new(s) else { continue };
        let keys: Vec<String> =
            cab.folder_entries().flat_map(|f| f.file_entries()).map(|f| f.name().to_owned()).collect();
        for key in keys {
            let Some(name) = png_name(names.get(&key).unwrap_or(&key)) else { continue };
            let mut png = Vec::new();
            cab.read_file(&key)?.take(MAX_PNG).read_to_end(&mut png)?;
            if png.starts_with(PNG_MAGIC) {
                std::fs::write(out.join(&name), png)?;
                written.insert(name.to_ascii_lowercase());
            }
        }
    }
    Ok(written.len())
}

/// The file name of a File-table name (`SHORT~1.PNG|Long Name.png`) if it is a PNG.
fn png_name(name: &str) -> Option<String> {
    let long = name.rsplit('|').next()?;
    let base = long.rsplit(['/', '\\']).next()?;
    (!base.is_empty() && base.to_ascii_lowercase().ends_with(".png")).then(|| base.to_owned())
}

/// MSI stream names pack two 6-bit characters into each UTF-16 unit; U+4840 marks a table.
fn decode_stream_name(name: &str) -> String {
    const A: &[u8] = b"0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz._";
    let mut s = String::new();
    for c in name.chars() {
        match c as usize {
            v @ 0x3800..0x4800 => s.extend([A[(v - 0x3800) & 63] as char, A[((v - 0x3800) >> 6) & 63] as char]),
            v @ 0x4800..0x4840 => s.push(A[v - 0x4800] as char),
            _ => s.push(c),
        }
    }
    s
}

/// File key -> `FileName` column, from the MSI's File table (empty if it can't be read). Tables
/// are column-major; string cells are 2-byte (3 when the string pool's flag says so) pool
/// indices, integer cells 2 or 4 bytes. The File table's column order and widths come from the
/// `_Columns` table (UA's installer has `Sequence` 4 bytes wide and `FileSize` before `Version`).
fn file_table(cfb: &mut cfb::CompoundFile<std::fs::File>) -> HashMap<String, String> {
    let entries: Vec<(String, PathBuf)> =
        cfb.walk().filter(|e| e.is_stream()).map(|e| (decode_stream_name(e.name()), e.path().to_path_buf())).collect();
    let mut read = |want: &str| -> Option<Vec<u8>> {
        let (_, p) = entries.iter().find(|(n, _)| n.strip_prefix('\u{4840}') == Some(want))?;
        let mut v = Vec::new();
        cfb.open_stream(p).ok()?.read_to_end(&mut v).ok()?;
        Some(v)
    };
    let (Some(pool), Some(data), Some(columns), Some(file)) =
        (read("_StringPool"), read("_StringData"), read("_Columns"), read("File"))
    else {
        return HashMap::new();
    };
    let w = if pool.get(3).is_some_and(|b| b & 0x80 != 0) { 3 } else { 2 };
    // Pool entries are (length u16, refcount u16); a string over 64 KiB (UA's licence text) has
    // length 0 with a non-zero refcount and its u32 length in the next entry.
    let mut strings = vec![String::new()];
    let (mut at, mut i) = (0, 4);
    while i + 4 <= pool.len() {
        let (mut len, refs) =
            (u16::from_le_bytes([pool[i], pool[i + 1]]) as usize, u16::from_le_bytes([pool[i + 2], pool[i + 3]]));
        i += 4;
        if len == 0 && refs != 0 && i + 4 <= pool.len() {
            len = u32::from_le_bytes([pool[i], pool[i + 1], pool[i + 2], pool[i + 3]]) as usize;
            i += 4;
        }
        let end = (at + len).min(data.len());
        strings.push(String::from_utf8_lossy(&data[at..end]).into_owned());
        at = end;
    }
    let cell = |table: &[u8], offset: usize, width: usize| -> usize {
        table.get(offset..offset + width).map_or(0, |b| b.iter().rev().fold(0, |a, &b| a << 8 | b as usize))
    };
    // `_Columns`: Table (string), Number (i2), Name (string), Type (i2). Type: 0x0800 = string,
    // else the low byte is the integer's width; the stored value carries 0x8000.
    let n = columns.len() / (2 * w + 4);
    let mut file_columns: Vec<(usize, String, usize)> = (0..n)
        .filter(|&r| strings.get(cell(&columns, r * w, w)).is_some_and(|t| t == "File"))
        .map(|r| {
            let number = cell(&columns, n * w + r * 2, 2).wrapping_sub(0x8000);
            let name = strings.get(cell(&columns, n * w + n * 2 + r * w, w)).cloned().unwrap_or_default();
            let ty = cell(&columns, n * w + n * 2 + n * w + r * 2, 2);
            let width = if ty & 0x0800 != 0 { w } else { (ty & 0xff).clamp(2, 4) };
            (number, name, width)
        })
        .collect();
    file_columns.sort_by_key(|c| c.0);
    let row = file_columns.iter().map(|c| c.2).sum::<usize>();
    if row == 0 || !file.len().is_multiple_of(row) {
        return HashMap::new();
    }
    let rows = file.len() / row;
    let offset_of = |want: &str| -> Option<(usize, usize)> {
        let mut off = 0;
        for (_, name, width) in &file_columns {
            if name == want {
                return Some((off, *width));
            }
            off += width * rows;
        }
        None
    };
    let (Some(key), Some(name)) = (offset_of("File"), offset_of("FileName")) else { return HashMap::new() };
    (0..rows)
        .map(|r| {
            let s = |(off, width): (usize, usize)| {
                strings.get(cell(&file, off + r * width, width)).cloned().unwrap_or_default()
            };
            (s(key), s(name))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::io::Write;

    fn png(tag: u8) -> Vec<u8> {
        [PNG_MAGIC, &[tag; 8]].concat()
    }

    /// The inverse of `decode_stream_name`, for the table streams.
    fn encode_table(name: &str) -> String {
        let idx = |c: char| "0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz._".find(c).unwrap() as u32;
        let cs: Vec<char> = name.chars().collect();
        let units = cs.chunks(2).map(|p| match p {
            [a, b] => 0x3800 + idx(*a) + (idx(*b) << 6),
            [a] => 0x4800 + idx(*a),
            _ => unreachable!(),
        });
        std::iter::once('\u{4840}').chain(units.map(|u| char::from_u32(u).unwrap())).collect()
    }

    /// A cached-MSI lookalike: `members` (cabinet key, bytes) in one MsZip cabinet stream beside a
    /// decoy, and, when `table` is set, a File table mapping each key to its `FileName` column.
    fn fake_msi(path: &Path, members: &[(&str, Vec<u8>)], table: Option<&[(&str, &str)]>) {
        let mut b = cab::CabinetBuilder::new();
        let f = b.add_folder(cab::CompressionType::MsZip);
        f.add_file("driver.sys");
        for (k, _) in members {
            f.add_file(*k);
        }
        let mut cw = b.build(io::Cursor::new(Vec::new())).unwrap();
        while let Some(mut file) = cw.next_file().unwrap() {
            let body = members.iter().find(|(k, _)| *k == file.file_name()).map_or(vec![7u8; 2048], |m| m.1.clone());
            file.write_all(&body).unwrap();
        }
        let cabinet = cw.finish().unwrap().into_inner();
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        let mut c = cfb::create(path).unwrap();
        c.create_stream("/Data1.cab").unwrap().write_all(&cabinet).unwrap();
        if let Some(rows) = table {
            // String pool: header (codepage), then (length, refcount) per string; index 0 is null.
            // The File table's columns (as UA's installer lays them out: FileSize before Version,
            // Sequence 4 bytes wide), named in the pool after the rows' strings.
            let columns: [(&str, u16); 8] = [
                ("File", 0xad48),
                ("Component_", 0x8d48),
                ("FileName", 0x8fff),
                ("FileSize", 0x8104),
                ("Version", 0x9d48),
                ("Language", 0x9d14),
                ("Attributes", 0x9502),
                ("Sequence", 0x8104),
            ];
            let strs: Vec<&str> = rows
                .iter()
                .flat_map(|(k, n)| [*k, *n])
                .chain(["comp", "1", "File"])
                .chain(columns.iter().map(|c| c.0))
                .collect();
            let mut pool = vec![0u8; 4];
            let mut data = Vec::new();
            for s in &strs {
                pool.extend((s.len() as u16).to_le_bytes());
                pool.extend(1u16.to_le_bytes());
                data.extend(s.as_bytes());
            }
            let comp = 2 * rows.len() as u16 + 1;
            let table_name = comp + 2;
            let blank = 0u16;
            let n = rows.len();
            let mut file = Vec::new();
            // Column-major in the order above: File, Component_, FileName (strings), FileSize
            // (4), Version, Language (strings), Attributes (2), Sequence (4).
            for (col, (_, ty)) in columns.iter().enumerate() {
                for r in 0..n {
                    match col {
                        0 => file.extend((1 + 2 * r as u16).to_le_bytes()),
                        1 => file.extend(comp.to_le_bytes()),
                        2 => file.extend((2 + 2 * r as u16).to_le_bytes()),
                        _ if ty & 0x0800 != 0 => file.extend(blank.to_le_bytes()),
                        _ => file.extend(vec![0u8; (ty & 0xff) as usize]),
                    }
                }
            }
            // `_Columns`: Table, Number, Name, Type, column-major, integers stored with 0x8000.
            let mut cols = Vec::new();
            for _ in &columns {
                cols.extend(table_name.to_le_bytes());
            }
            for (i, _) in columns.iter().enumerate() {
                cols.extend((0x8000 + 1 + i as u16).to_le_bytes());
            }
            for (i, _) in columns.iter().enumerate() {
                cols.extend((table_name + 1 + i as u16).to_le_bytes());
            }
            for (_, ty) in &columns {
                cols.extend(ty.to_le_bytes());
            }
            for (name, bytes) in [("_StringPool", pool), ("_StringData", data), ("_Columns", cols), ("File", file)] {
                c.create_stream(format!("/{}", encode_table(name))).unwrap().write_all(&bytes).unwrap();
            }
        }
        c.flush().unwrap();
    }

    #[test]
    fn extracts_pngs_from_a_cached_msi_with_real_names() {
        let t = std::env::temp_dir().join(format!("oa-assets-icons-{}", std::process::id()));
        let _ = fs::remove_dir_all(&t);
        let msi = t.join("cache/apollo-32bit.msi");
        fake_msi(
            &msi,
            &[
                ("UAD_1176.png", png(1)),
                ("Pultec_EQ.png", png(2)),
                ("notes.txt", b"hi".to_vec()),
                ("fake.png", vec![0; 9]),
            ],
            Some(&[
                ("UAD_1176.png", "UAD_11~1.PNG|UAD 1176LN.png"),
                ("Pultec_EQ.png", "PULTEC~1.PNG|UAD Pultec EQP-1A.png"),
                ("notes.txt", "notes.txt"),
                ("fake.png", "fake.png"),
            ]),
        );
        let data = t.join("data");
        let stale = data.join(CONSOLE_DIR).join(DIR);
        fs::create_dir_all(&stale).unwrap();
        fs::write(stale.join("old.png"), png(9)).unwrap();

        let got = import(&PluginIcons::CachedMsi(msi), &data).unwrap();
        assert_eq!((got.count, &got.dest), (2, &stale));
        let mut files: Vec<_> =
            fs::read_dir(&stale).unwrap().map(|e| e.unwrap().file_name().into_string().unwrap()).collect();
        files.sort();
        assert_eq!(files, ["UAD 1176LN.png", "UAD Pultec EQP-1A.png"]);
        assert_eq!(fs::read(stale.join("UAD 1176LN.png")).unwrap(), png(1));
        let m = crate::import::read_manifest(&data);
        assert_eq!(m["plugin_icons"]["count"], 2);
        assert!(m["plugin_icons"]["from"].as_str().unwrap().ends_with("apollo-32bit.msi"));
        fs::remove_dir_all(&t).unwrap();
    }

    #[test]
    fn without_a_file_table_the_cabinet_keys_are_the_names() {
        let t = std::env::temp_dir().join(format!("oa-assets-icons-keys-{}", std::process::id()));
        let _ = fs::remove_dir_all(&t);
        let msi = t.join("apollo-32bit.msi");
        fake_msi(&msi, &[("UAD_Fairchild.PNG", png(3)), ("readme.txt", b"x".to_vec())], None);
        let got = import(&PluginIcons::CachedMsi(msi), &t.join("data")).unwrap();
        assert_eq!(got.count, 1);
        assert!(got.dest.join("UAD_Fairchild.PNG").is_file());
        fs::remove_dir_all(&t).unwrap();
    }

    #[test]
    fn copies_an_installed_folder_and_fails_on_an_empty_source() {
        let t = std::env::temp_dir().join(format!("oa-assets-icons-dir-{}", std::process::id()));
        let _ = fs::remove_dir_all(&t);
        let src = t.join("Plug-In Icons");
        fs::create_dir_all(src.join("sub")).unwrap();
        fs::write(src.join("UAD 1176LN.png"), png(1)).unwrap();
        fs::write(src.join("Thumbs.db"), b"x").unwrap();
        let data = t.join("data");
        let got = import(&PluginIcons::Folder(src.clone()), &data).unwrap();
        assert_eq!(got.count, 1);
        assert!(got.dest.join("UAD 1176LN.png").is_file());
        assert!(!got.dest.join("Thumbs.db").exists());
        assert!(!got.dest.with_file_name(format!("{DIR}.partial")).exists());

        // Nothing to import: an error, and the earlier import stays.
        fs::remove_file(src.join("UAD 1176LN.png")).unwrap();
        assert!(import(&PluginIcons::Folder(src), &data).is_err());
        assert!(got.dest.join("UAD 1176LN.png").is_file());
        fs::remove_dir_all(&t).unwrap();
    }
}

#[cfg(test)]
mod real_msi {
    //! `OA_REAL_MSI=/path/to/apollo-32bit.msi cargo test -p oa-assets real_msi -- --nocapture`:
    //! what the File-table reader sees in a real installer (never committed, never shipped).
    use super::*;

    #[test]
    fn report() {
        let Ok(path) = std::env::var("OA_REAL_MSI") else { return };
        let mut cfb = cfb::open(&path).expect("open msi");
        let names: Vec<String> = cfb.walk().filter(|e| e.is_stream()).map(|e| decode_stream_name(e.name())).collect();
        eprintln!("streams: {:?}", names.iter().map(|n| n.replace('\u{4840}', "#")).collect::<Vec<_>>());
        let table = file_table(&mut cfb);
        eprintln!("file table rows: {}", table.len());
        for (k, v) in table.iter().take(3) {
            eprintln!("  {k} -> {v}");
        }
        let out = std::env::temp_dir().join("oa-real-msi-icons");
        let _ = std::fs::remove_dir_all(&out);
        std::fs::create_dir_all(&out).unwrap();
        eprintln!("extracted: {:?}", extract_msi(Path::new(&path), &out));
    }
}
