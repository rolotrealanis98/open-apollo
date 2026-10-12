//! Command line.

use std::fmt;
use std::path::PathBuf;
use std::str::FromStr;

use crate::view::ViewMode;

pub const USAGE: &str = "\
uaconsole — a native UAD Console for open-apollo

usage:
  uaconsole [--compact] [--fit|--1to1] [--engine host[:port]] [--mock snapshot.json]
            [--preview] [--writes] [--live-meters] [--stats] [--open-plugin STRIP:SLOT]
  uaconsole --render out.png [--compact] [--scale N] [--size WxH] [--view SPEC] [--menu SPEC] [--menu-bar]
                                                         headless render (preview data);
                                                         --size is the window in points
  uaconsole --bench N [--compact] [--scale N]            render N frames headless, print timings

  --view sets the mixer view at start (window and --render), comma-separated items:
  inputs|inserts|sends|fader=0|1, small|large, fixed=0|1, overview=0|1, scroll=F, aux-scroll=F,
  browser=0|1, tab=plugins|presets, hint=0|1, aux=0|1, ctrl=0|1, scenes=0|1, scene-info=0|1,
  first=N, assign=STRIP:SLOT (e.g. --view ctrl=1,overview=1,small).
  --stats starts with the F1 overlay; --open-plugin in0:0 (or in0:unison) opens that insert's
  panel at start (window and --render). --menu opens a Console menu in --render: bar:N,
  insert:STRIP:SLOT, label:STRIP, solo:STRIP, meter:STRIP, preamp:STRIP, browser:N, then >I
  to open submenu entry I; --menu-bar draws Windows Console's in-window menu bar.

window keys: Ctrl+Q quit · F11 fullscreen · Ctrl+0/=/- zoom · Ctrl+1 fit/1:1 · Tab compact ·
             F1 stats overlay · Esc close panel · wheel pans (or steps a knob/fader under it)
             right click / long press: Console's context menus; Console's own shortcuts
             (Ctrl+Z, Ctrl+, settings, Ctrl+A auxes, Ctrl+R control room, ←/→ scroll, …)

Without --engine/--mock the window connects to the local engine (127.0.0.1:4710); --preview
forces preview data. Engine writes (set/post) stay off unless --writes is given.";

pub const DEFAULT_ENGINE_PORT: u16 = 4710;

/// `host[:port]` (IPv6 hosts in brackets).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EngineAddr {
    pub host: String,
    pub port: u16,
}

impl EngineAddr {
    /// The open-apollo daemon on this machine.
    pub fn local() -> Self {
        Self {
            host: "127.0.0.1".into(),
            port: DEFAULT_ENGINE_PORT,
        }
    }
}

impl FromStr for EngineAddr {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, String> {
        let s = s.trim();
        let bad = || format!("bad engine address {s:?} (expected host[:port])");
        let (host, port) = if let Some(rest) = s.strip_prefix('[') {
            let (host, tail) = rest.split_once(']').ok_or_else(bad)?;
            match tail.strip_prefix(':') {
                Some(p) => (host, p.parse().map_err(|_| bad())?),
                None if tail.is_empty() => (host, DEFAULT_ENGINE_PORT),
                None => return Err(bad()),
            }
        } else {
            match s.rsplit_once(':') {
                Some((h, p)) if !h.contains(':') => (h, p.parse().map_err(|_| bad())?),
                _ => (s, DEFAULT_ENGINE_PORT),
            }
        };
        if host.is_empty() {
            return Err(bad());
        }
        Ok(Self {
            host: host.to_string(),
            port,
        })
    }
}

impl fmt::Display for EngineAddr {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.host.contains(':') {
            write!(f, "[{}]:{}", self.host, self.port)
        } else {
            write!(f, "{}:{}", self.host, self.port)
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Args {
    pub render: Option<PathBuf>,
    pub bench: Option<usize>,
    pub scale: Option<f32>,
    /// Window size in points for `--render` (Console lays out to its window).
    pub size: Option<(f32, f32)>,
    pub compact: bool,
    pub view: Option<ViewMode>,
    pub engine: Option<EngineAddr>,
    pub mock: Option<PathBuf>,
    pub preview: bool,
    pub writes: bool,
    pub live_meters: bool,
    pub stats: bool,
    /// (strip id, slot) to open at start.
    pub open_plugin: Option<(String, u8)>,
    /// Mixer view spec (`rack::ViewState::apply_spec`).
    pub view_spec: Option<String>,
    /// `--menu SPEC` (`ViewState::open_menu_spec`) for `--render`.
    pub menu: Option<String>,
    /// `--menu-bar`: the in-window menu bar in `--render`.
    pub menu_bar: bool,
    pub help: bool,
}

impl Args {
    pub fn parse(args: impl IntoIterator<Item = String>) -> Result<Self, String> {
        let mut out = Args::default();
        let mut it = args.into_iter();
        while let Some(a) = it.next() {
            let mut value = |name: &str| it.next().ok_or_else(|| format!("{name} needs a value"));
            match a.as_str() {
                "--render" => out.render = Some(value("--render")?.into()),
                "--bench" => {
                    out.bench = Some(
                        value("--bench")?
                            .parse()
                            .map_err(|_| "--bench needs a frame count")?,
                    )
                }
                "--scale" => {
                    out.scale = Some(
                        value("--scale")?
                            .parse()
                            .map_err(|_| "--scale needs a number")?,
                    )
                }
                "--size" => out.size = Some(parse_size(&value("--size")?)?),
                "--compact" => out.compact = true,
                "--fit" => out.view = Some(ViewMode::Fit),
                "--1to1" | "--1:1" => out.view = Some(ViewMode::OneToOne),
                "--engine" => out.engine = Some(value("--engine")?.parse()?),
                "--mock" => out.mock = Some(value("--mock")?.into()),
                "--preview" => out.preview = true,
                "--writes" => out.writes = true,
                "--live-meters" => out.live_meters = true,
                "--stats" => out.stats = true,
                "--open-plugin" => out.open_plugin = Some(parse_slot(&value("--open-plugin")?)?),
                "--view" => {
                    let spec = value("--view")?;
                    rack::ViewState::default().apply_spec(&spec)?;
                    out.view_spec = Some(spec);
                }
                "--menu" => out.menu = Some(value("--menu")?),
                "--menu-bar" => out.menu_bar = true,
                "-h" | "--help" => out.help = true,
                other => return Err(format!("unknown argument {other:?}")),
            }
        }
        if out.engine.is_some() && out.mock.is_some() {
            return Err("--engine and --mock are exclusive".into());
        }
        Ok(out)
    }
}

/// `WxH` in points, e.g. `1470x1105`.
fn parse_size(s: &str) -> Result<(f32, f32), String> {
    let bad = || format!("bad --size {s:?} (expected WxH in points, e.g. 1470x1105)");
    let (w, h) = s.split_once(['x', 'X']).ok_or_else(bad)?;
    let (w, h): (f32, f32) = (w.trim().parse().map_err(|_| bad())?, h.trim().parse().map_err(|_| bad())?);
    if !(w > 0.0 && h > 0.0) {
        return Err(bad());
    }
    Ok((w, h))
}

/// `strip:slot` with a 0-based slot or `unison`.
fn parse_slot(s: &str) -> Result<(String, u8), String> {
    let bad = || format!("bad --open-plugin {s:?} (expected strip:slot, e.g. in0:0 or in0:unison)");
    let (strip, slot) = s.rsplit_once(':').ok_or_else(bad)?;
    let slot = if slot.eq_ignore_ascii_case("unison") {
        crate::overlay::UNISON_SLOT
    } else {
        slot.parse().map_err(|_| bad())?
    };
    if strip.is_empty() {
        return Err(bad());
    }
    Ok((strip.to_string(), slot))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(s: &str) -> Result<Args, String> {
        Args::parse(s.split_whitespace().map(String::from))
    }

    #[test]
    fn engine_addr() {
        assert_eq!(
            "10.0.0.5".parse::<EngineAddr>().unwrap(),
            EngineAddr {
                host: "10.0.0.5".into(),
                port: 4710
            }
        );
        assert_eq!(
            "apollo.local:4711".parse::<EngineAddr>().unwrap().port,
            4711
        );
        assert_eq!(
            "[::1]:9".parse::<EngineAddr>().unwrap(),
            EngineAddr {
                host: "::1".into(),
                port: 9
            }
        );
        assert_eq!("::1".parse::<EngineAddr>().unwrap().host, "::1");
        assert!("host:notaport".parse::<EngineAddr>().is_err());
        assert!(":4710".parse::<EngineAddr>().is_err());
        assert_eq!(
            EngineAddr {
                host: "::1".into(),
                port: 9
            }
            .to_string(),
            "[::1]:9"
        );
    }

    #[test]
    fn flags() {
        assert_eq!(parse("--size 1470x1105").unwrap().size, Some((1470.0, 1105.0)));
        assert!(parse("--size 1470").is_err());
        let a = parse("--compact --1to1 --engine studio:4710 --writes").unwrap();
        assert!(a.compact && a.writes);
        assert_eq!(a.view, Some(ViewMode::OneToOne));
        assert_eq!(a.engine.unwrap().host, "studio");
        assert_eq!(parse("--bench 50 --scale 1.5").unwrap().bench, Some(50));
        assert!(parse("--bench").is_err());
        assert!(parse("--nope").is_err());
        assert!(parse("--engine a --mock b.json").is_err());
        assert_eq!(
            parse("--open-plugin in2:3").unwrap().open_plugin,
            Some(("in2".into(), 3))
        );
        assert_eq!(
            parse("--open-plugin in0:UNISON").unwrap().open_plugin,
            Some(("in0".into(), 255))
        );
        assert!(parse("--open-plugin in0").is_err());
        assert_eq!(parse("--view ctrl=1,overview=1").unwrap().view_spec.as_deref(), Some("ctrl=1,overview=1"));
        assert!(parse("--view nope=1").is_err());
    }
}
