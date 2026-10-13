//! Command line and where things are.

use std::path::{Path, PathBuf};

use crate::steps::StepId;

pub const USAGE: &str = "\
open-apollo-setup: set up open-apollo with your own UAD install

USAGE:
  open-apollo-setup [--demo] [--data DIR] [--repo DIR] [--step STEP] [--auto] [--window-size WxH]

  --demo        Simulate the privileged steps (DSP programs & firmware, driver)
                and the Console build: fake log lines, no pkexec, no builds.
                Scanning and the Console art import still run for real (read-only
                toward your UAD install; the import writes only to --data).
  --data DIR    Import Console art into DIR instead of ~/.local/share/open-apollo
  --repo DIR    The open-apollo checkout holding scripts/install.sh
                (default: found from this program's or the current folder)
  --step STEP   Open on STEP: welcome, system, find, import, firmware, driver,
                console, done
  --auto        With --demo: walk through the steps by itself, stopping at --step
                (default: done). For showing the flow and taking screenshots.
  --window-size WxH  Initial window size in points (default 1040x720)
  -h, --help    Show this help

Environment: OA_SETUP_EXTRACTOR  the firmware extractor program (default: oa-assets)";

#[derive(Clone, Debug, PartialEq)]
pub struct Options {
    pub demo: bool,
    pub data: PathBuf,
    /// `data` is the default folder, which uaconsole finds by itself.
    pub data_is_default: bool,
    pub repo: Option<PathBuf>,
    pub start: StepId,
    /// `--auto`: walk through the steps by itself and stop at this one.
    pub auto_until: Option<StepId>,
    pub window_size: [f32; 2],
}

#[derive(Debug, PartialEq)]
pub enum Parsed {
    Run(Options),
    Help,
}

pub fn parse(args: impl IntoIterator<Item = String>) -> Result<Parsed, String> {
    let mut it = args.into_iter();
    let mut demo = false;
    let mut data = None;
    let mut repo = None;
    let mut step = None;
    let mut auto = false;
    let mut window_size = [1040.0, 720.0];
    while let Some(a) = it.next() {
        let (flag, inline) = match a.split_once('=') {
            Some((f, v)) if f.starts_with("--") => (f.to_string(), Some(v.to_string())),
            _ => (a.clone(), None),
        };
        let mut value = |name: &str| inline.clone().or_else(|| it.next()).ok_or(format!("{name} needs a value"));
        match flag.as_str() {
            "--demo" => demo = true,
            "--auto" => auto = true,
            "--data" => data = Some(PathBuf::from(value("--data")?)),
            "--repo" => repo = Some(PathBuf::from(value("--repo")?)),
            "--step" => {
                let v = value("--step")?;
                step = Some(StepId::parse(&v).ok_or(format!("unknown step {v}"))?);
            }
            "--window-size" => {
                let v = value("--window-size")?;
                window_size = parse_wxh(&v).ok_or(format!("--window-size takes WxH, e.g. 1040x900, not {v}"))?;
            }
            "-h" | "--help" => return Ok(Parsed::Help),
            other => return Err(format!("unknown option {other}")),
        }
    }
    if auto && !demo {
        return Err("--auto only works with --demo".into());
    }
    let default_data = oa_assets::data_dir();
    let data = data.map(absolute).unwrap_or_else(|| default_data.clone());
    let data_is_default = data == default_data;
    let repo = match repo {
        Some(r) if is_repo(&r) => Some(absolute(r)),
        Some(r) => return Err(format!("{} isn't an open-apollo checkout (no scripts/install.sh)", r.display())),
        None => find_repo(),
    };
    let (start, auto_until) = match auto {
        true => (StepId::Welcome, Some(step.unwrap_or(StepId::Done))),
        false => (step.unwrap_or(StepId::Welcome), None),
    };
    Ok(Parsed::Run(Options { demo, data, data_is_default, repo, start, auto_until, window_size }))
}

/// `1040x900` -> [1040.0, 900.0]; at least the window's minimum size.
fn parse_wxh(s: &str) -> Option<[f32; 2]> {
    let (w, h) = s.split_once(['x', 'X'])?;
    let (w, h): (f32, f32) = (w.trim().parse().ok()?, h.trim().parse().ok()?);
    (w >= 860.0 && h >= 560.0 && w <= 8192.0 && h <= 8192.0).then_some([w, h])
}

fn absolute(p: PathBuf) -> PathBuf {
    if p.is_absolute() { p } else { std::env::current_dir().map(|d| d.join(&p)).unwrap_or(p) }
}

pub fn is_repo(p: &Path) -> bool {
    p.join("scripts/install.sh").is_file() && p.join("console-rs").is_dir()
}

/// `$OPEN_APOLLO_REPO`, else the checkout this program was built in or run from.
pub fn find_repo() -> Option<PathBuf> {
    if let Some(r) = std::env::var_os("OPEN_APOLLO_REPO").map(PathBuf::from).filter(|r| is_repo(r)) {
        return Some(r);
    }
    let starts = [std::env::current_exe().ok(), std::env::current_dir().ok()];
    starts.into_iter().flatten().find_map(|s| s.ancestors().find(|a| is_repo(a)).map(Path::to_path_buf))
}

/// The installed uaconsole: on PATH, or where install-uaconsole.sh puts it.
pub fn installed_console() -> Option<PathBuf> {
    crate::syscheck::in_path("uaconsole").or_else(|| {
        let p = PathBuf::from(std::env::var_os("HOME")?).join(".local/bin/uaconsole");
        p.is_file().then_some(p)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(args: &[&str]) -> Result<Parsed, String> {
        parse(args.iter().map(|s| s.to_string()))
    }

    #[test]
    fn parses_flags() {
        let Parsed::Run(o) = run(&["--demo", "--data", "/tmp/x", "--step", "driver"]).unwrap() else { panic!() };
        assert!(o.demo && o.auto_until.is_none());
        assert_eq!(o.data, PathBuf::from("/tmp/x"));
        assert!(!o.data_is_default);
        assert_eq!(o.start, StepId::Driver);
        let Parsed::Run(o) = run(&["--data=/tmp/y", "--step=find"]).unwrap() else { panic!() };
        assert_eq!((o.data, o.start), (PathBuf::from("/tmp/y"), StepId::Find));
        assert_eq!(run(&["--help"]), Ok(Parsed::Help));
        assert!(run(&["--step", "bogus"]).is_err());
        assert!(run(&["--data"]).is_err());
        assert!(run(&["--auto"]).is_err(), "--auto needs --demo");
        assert!(run(&["--repo", "/nonexistent"]).is_err());
        let Parsed::Run(o) = run(&["--window-size", "1100x900"]).unwrap() else { panic!() };
        assert_eq!(o.window_size, [1100.0, 900.0]);
        assert!(run(&["--window-size", "100x100"]).is_err());
        assert!(run(&["--window-size=wide"]).is_err());
        let Parsed::Run(o) = run(&["--demo", "--auto", "--step", "console"]).unwrap() else { panic!() };
        assert_eq!((o.start, o.auto_until), (StepId::Welcome, Some(StepId::Console)), "--auto walks from the start");
        let Parsed::Run(o) = run(&["--demo", "--auto"]).unwrap() else { panic!() };
        assert_eq!(o.auto_until, Some(StepId::Done));
    }

    #[test]
    fn finds_this_checkout() {
        let here = Path::new(env!("CARGO_MANIFEST_DIR"));
        let repo = here.ancestors().find(|a| is_repo(a));
        assert!(repo.is_some(), "the crate lives in an open-apollo checkout");
    }
}
