//! The "DSP programs & firmware" step.
//!
//! The driver needs UA's DSP programs and device firmware. open-apollo ships no
//! UA bytes, so they're extracted from the user's own Windows install into
//! [`FIRMWARE_DIR`] by a privileged extractor run through pkexec. The extractor
//! is developed separately; this module only knows its command line
//! (`<program> firmware --from ROOT --out DIR`) and its output: one line per
//! file, `NAME  STATUS  [sha256 HEX]  [note]`, plus the installers' usual
//! `[ OK ]`/`[WARN]` lines. Point `OA_SETUP_EXTRACTOR` at another program to swap it.

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::Command;

use crate::steps::Status;

/// Where the extracted files go; the driver's `request_firmware` looks under /lib/firmware.
pub const FIRMWARE_DIR: &str = "/lib/firmware/universal-audio";
pub const MANIFEST: &str = "manifest.json";
/// Overrides the extractor program.
pub const EXTRACTOR_ENV: &str = "OA_SETUP_EXTRACTOR";

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Extractor {
    pub program: PathBuf,
}

impl Extractor {
    /// `$OA_SETUP_EXTRACTOR`, else `oa-assets` next to this program (same cargo
    /// target dir or install prefix), else `oa-assets` on PATH.
    pub fn locate() -> Option<Extractor> {
        if let Some(p) = std::env::var_os(EXTRACTOR_ENV).filter(|v| !v.is_empty()) {
            return Some(Extractor { program: PathBuf::from(p) });
        }
        let sibling = std::env::current_exe().ok().and_then(|e| e.parent().map(|d| d.join("oa-assets")));
        if let Some(s) = sibling.filter(|s| s.is_file()) {
            return Some(Extractor { program: s });
        }
        crate::syscheck::in_path("oa-assets").map(|program| Extractor { program })
    }

    pub fn args(&self, from: &Path, out: &Path) -> Vec<OsString> {
        vec!["firmware".into(), "--from".into(), from.into(), "--out".into(), out.into()]
    }

    /// The command as the user would type it (for the log and a terminal fallback).
    pub fn command_line(&self, from: &Path, out: &Path) -> String {
        let mut parts = vec!["pkexec".to_string(), shell_quote(&self.program.to_string_lossy())];
        parts.extend(self.args(from, out).iter().map(|a| shell_quote(&a.to_string_lossy())));
        parts.join(" ")
    }

    /// Whether the program has a `firmware` command yet. Runs its help, unprivileged.
    pub fn supports_firmware(&self) -> bool {
        Command::new(&self.program)
            .arg("help")
            .output()
            .map(|o| {
                let text = [o.stdout, o.stderr].concat();
                String::from_utf8_lossy(&text).lines().any(|l| l.trim_start().contains(" firmware"))
            })
            .unwrap_or(false)
    }
}

pub fn shell_quote(s: &str) -> String {
    if !s.is_empty() && s.chars().all(|c| c.is_ascii_alphanumeric() || "-_./=:@+,".contains(c)) {
        s.to_string()
    } else {
        format!("'{}'", s.replace('\'', r"'\''"))
    }
}

/// One extracted file as the extractor reports it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FileLine {
    pub name: String,
    pub status: Status,
    /// The extractor said `missing` (not in the install), not `warn`.
    pub missing: bool,
    pub sha256: Option<String>,
    pub note: Option<String>,
}

/// `ua-dsp-programs.bin  ok  sha256 3f1a…  (from 118 DLLs)`; `None` for other lines.
pub fn parse_file_line(line: &str) -> Option<FileLine> {
    let clean = crate::installlog::strip_ansi(line);
    let mut tok = clean.split_whitespace();
    let name = tok.next()?;
    if !name.contains('.') || name.starts_with('[') {
        return None;
    }
    let word = tok.next()?.to_ascii_lowercase();
    let status = match word.as_str() {
        "ok" => Status::Done,
        "skip" | "skipped" | "unchanged" => Status::Skipped,
        "missing" | "warn" => Status::Warning,
        "fail" | "failed" | "error" => Status::Failed,
        _ => return None,
    };
    let mut rest: Vec<&str> = tok.collect();
    let mut sha256 = None;
    if rest.first() == Some(&"sha256") && rest.len() >= 2 {
        sha256 = Some(rest[1].to_string());
        rest.drain(..2);
    }
    let note = (!rest.is_empty()).then(|| rest.join(" "));
    Some(FileLine { name: name.to_string(), status, missing: word == "missing", sha256, note })
}

/// What a previous extraction left in `dir`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Existing {
    pub files: usize,
    pub source: Option<String>,
}

/// Read `dir/manifest.json` from an earlier run, if any (read-only).
pub fn existing(dir: &Path) -> Option<Existing> {
    let bytes = std::fs::read(dir.join(MANIFEST)).ok()?;
    let v: serde_json::Value = serde_json::from_slice(&bytes).ok()?;
    let files = match v.get("files") {
        Some(serde_json::Value::Array(a)) => a.len(),
        Some(serde_json::Value::Object(o)) => o.len(),
        _ => v.as_object().map(|o| o.len()).unwrap_or(0),
    };
    let source = ["from", "source"].iter().find_map(|k| v.get(*k).and_then(|s| s.as_str())).map(str::to_string);
    Some(Existing { files, source })
}

/// A USB Apollo's boot image (`ApolloSolo.bin`, `ApolloTwin.bin`, `ApolloTwinX.bin`).
pub fn is_usb_image(name: &str) -> bool {
    !name.contains('/') && name.starts_with("Apollo") && name.ends_with(".bin")
}

/// A missing USB boot image on a computer without a USB Apollo: nothing needs it.
pub fn not_needed(f: &FileLine, usb_apollo: bool) -> bool {
    !usb_apollo && f.missing && is_usb_image(&f.name)
}

/// The overall result of an extractor run from its exit code and file lines.
/// `usb_apollo`: a USB Apollo (vendor 2b5a) is connected, so its boot image
/// matters; without one a missing `Apollo*.bin` doesn't make it a warning.
pub fn outcome(exit: Option<i32>, files: &[FileLine], usb_apollo: bool) -> Status {
    let bad = |f: &FileLine| (f.status == Status::Failed || f.status == Status::Warning) && !not_needed(f, usb_apollo);
    match exit {
        Some(0) if files.iter().any(bad) => Status::Warning,
        Some(0) if files.is_empty() => Status::Warning,
        Some(0) => Status::Done,
        _ => Status::Failed,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_per_file_lines() {
        assert_eq!(
            parse_file_line("ua-dsp-programs.bin  ok  sha256 3f1a9c  (5 programs from 118 DLLs)"),
            Some(FileLine {
                name: "ua-dsp-programs.bin".into(),
                status: Status::Done,
                missing: false,
                sha256: Some("3f1a9c".into()),
                note: Some("(5 programs from 118 DLLs)".into()),
            })
        );
        let missing = parse_file_line("ua-usb-twin.bin    missing   no Twin USB driver in this install").unwrap();
        assert_eq!((missing.status, missing.sha256), (Status::Warning, None));
        assert_eq!(missing.note.as_deref(), Some("no Twin USB driver in this install"));
        assert_eq!(parse_file_line("manifest.json  OK").unwrap().status, Status::Done);
        assert_eq!(parse_file_line("x.bin failed").unwrap().status, Status::Failed);
        assert_eq!(parse_file_line("[ OK ]  Wrote 4 files"), None);
        assert_eq!(parse_file_line("Reading /run/media/u/Windows"), None);
        assert_eq!(parse_file_line(""), None);
        assert_eq!(parse_file_line("foo.bin  maybe"), None);
    }

    #[test]
    fn outcome_from_exit_and_files() {
        let ok = parse_file_line("a.bin ok sha256 00").unwrap();
        let miss = parse_file_line("b.bin missing").unwrap();
        assert_eq!(outcome(Some(0), std::slice::from_ref(&ok), true), Status::Done);
        assert_eq!(outcome(Some(0), &[ok.clone(), miss], true), Status::Warning);
        assert_eq!(outcome(Some(0), &[], true), Status::Warning, "nothing reported is suspicious");
        assert_eq!(outcome(Some(2), &[ok], true), Status::Failed);
        assert_eq!(outcome(None, &[], true), Status::Failed);
    }

    #[test]
    fn missing_usb_images_are_fine_without_a_usb_apollo() {
        let line = |l: &str| parse_file_line(l).unwrap();
        let dsp = line("dsp/0da.bill     ok       sha256 37cd  (UAD Mic Input Strip Header.dll +0x2e960)");
        let solo = line("ApolloSolo.bin   missing  not in this install (only USB Apollos need it)");
        let twin_x = line("ApolloTwinX.bin  missing  not in this install (only USB Apollos need it)");
        let files = [dsp.clone(), solo.clone(), twin_x];
        assert_eq!(outcome(Some(0), &files, false), Status::Done, "no USB Apollo: nothing needs the images");
        assert_eq!(outcome(Some(0), &files, true), Status::Warning, "a USB Apollo needs its image");
        // Only `missing` USB images are excused: not a `warn` one, not a DSP module.
        let unknown = line("ApolloTwin.bin   warn     sha256 ab12  (Firmware/USB) not a known version; written anyway");
        assert_eq!(outcome(Some(0), &[dsp.clone(), unknown], false), Status::Warning);
        let module = line("dsp/0eb.bill     missing  not in this install");
        assert_eq!(outcome(Some(0), &[dsp, module, solo], false), Status::Warning);
        assert_eq!(outcome(Some(1), &files, false), Status::Failed);
    }

    #[test]
    fn command_line_quotes_paths() {
        let x = Extractor { program: "/opt/oa/oa-assets".into() };
        assert_eq!(
            x.command_line(Path::new("/run/media/me/OS Disk"), Path::new(FIRMWARE_DIR)),
            "pkexec /opt/oa/oa-assets firmware --from '/run/media/me/OS Disk' --out /lib/firmware/universal-audio"
        );
        assert_eq!(shell_quote("it's"), r"'it'\''s'");
    }

    #[test]
    fn reads_an_earlier_manifest() {
        let t = std::env::temp_dir().join(format!("oa-setup-fw-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&t);
        std::fs::create_dir_all(&t).unwrap();
        assert_eq!(existing(&t), None);
        std::fs::write(t.join(MANIFEST), r#"{"from":"/mnt/win","files":[{"name":"a.bin"},{"name":"b.bin"}]}"#).unwrap();
        assert_eq!(existing(&t), Some(Existing { files: 2, source: Some("/mnt/win".into()) }));
        std::fs::remove_dir_all(&t).unwrap();
    }
}
