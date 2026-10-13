//! The "Import assets" step: a list of import tasks run one after another on a
//! background thread. Console art is the only one today; plug-in panels slot in
//! as another [`Item`] once their recipes exist.

use std::path::{Path, PathBuf};
use std::sync::mpsc::Sender;
use std::thread;

use oa_assets::detect::Install;
use oa_assets::import;

use crate::jobs::Waker;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Item {
    ConsoleArt,
    PluginPanels,
}

impl Item {
    pub const ALL: [Item; 2] = [Item::ConsoleArt, Item::PluginPanels];

    pub fn title(self) -> &'static str {
        match self {
            Item::ConsoleArt => "Console art",
            Item::PluginPanels => "Plug-in panels",
        }
    }

    pub fn description(self) -> &'static str {
        match self {
            Item::ConsoleArt => "The mixer artwork, layouts and fonts that UAD Console draws with.",
            Item::PluginPanels => "Editor panels for your Powered plug-ins, generated from your own plug-in files.",
        }
    }

    /// Whether this version can import it at all.
    pub fn available(self) -> bool {
        match self {
            Item::ConsoleArt => true,
            Item::PluginPanels => false,
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum State {
    Waiting,
    /// Not in this version yet.
    ComingSoon,
    /// The chosen install doesn't have it.
    NotInInstall(String),
    Copying {
        files: u64,
        bytes: u64,
        total_files: u64,
        total_bytes: u64,
    },
    Done {
        files: u64,
        bytes: u64,
        dest: PathBuf,
    },
    Failed(String),
}

impl State {
    pub fn fraction(&self) -> Option<f32> {
        match self {
            State::Copying { bytes, total_bytes, .. } if *total_bytes > 0 => Some(*bytes as f32 / *total_bytes as f32),
            State::Copying { .. } => Some(0.0),
            _ => None,
        }
    }

    pub fn is_busy(&self) -> bool {
        matches!(self, State::Copying { .. })
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum Msg {
    State(Item, State),
    Finished,
}

/// The initial state of every item for `install`.
pub fn plan(install: Option<&Install>) -> Vec<(Item, State)> {
    Item::ALL
        .into_iter()
        .map(|item| {
            let state = if !item.available() {
                State::ComingSoon
            } else if item == Item::ConsoleArt && install.is_some_and(|i| i.console.is_none()) {
                State::NotInInstall("This install has no UAD Console.".into())
            } else {
                State::Waiting
            };
            (item, state)
        })
        .collect()
}

/// Import every waiting item of `install` into `data`.
pub fn run(install: Install, data: PathBuf, items: Vec<Item>, tx: Sender<Msg>, wake: Waker) {
    thread::spawn(move || {
        for item in items {
            match item {
                Item::ConsoleArt => console_art(&install, &data, &tx, &wake),
                // Plug-in panels: once sprite recipes land, generate them here.
                Item::PluginPanels => {}
            }
        }
        let _ = tx.send(Msg::Finished);
        wake();
    });
}

fn console_art(install: &Install, data: &Path, tx: &Sender<Msg>, wake: &Waker) {
    let send = |s: State| {
        let _ = tx.send(Msg::State(Item::ConsoleArt, s));
        wake();
    };
    let Some(console) = &install.console else {
        send(State::NotInInstall("This install has no UAD Console.".into()));
        return;
    };
    let (total_files, total_bytes) = match import::measure(&console.resources) {
        Ok(t) => t,
        Err(e) => return send(State::Failed(format!("Couldn't read {}: {e}", console.resources.display()))),
    };
    send(State::Copying { files: 0, bytes: 0, total_files, total_bytes });
    // About 200 updates over the whole copy keeps the UI smooth without flooding it.
    let step = (total_bytes / 200).max(1);
    let mut next = step;
    let result = import::import_console_with(console, data, &mut |files, bytes| {
        if bytes >= next || files == total_files {
            next = bytes + step;
            send(State::Copying { files, bytes, total_files, total_bytes });
        }
    });
    match result {
        Ok(c) => send(State::Done { files: c.files, bytes: c.bytes, dest: c.dest }),
        Err(e) => send(State::Failed(format!("Copy failed: {e}"))),
    }
}

/// What an earlier import left: Console version and file count from the manifest.
pub fn previous(data: &Path) -> Option<(Option<String>, u64)> {
    let m = import::read_manifest(data);
    let c = m.get("console")?;
    if !data.join(import::CONSOLE_DIR).join("appearance/Appearance.json").is_file() {
        return None;
    }
    Some((c["version"].as_str().map(str::to_string), c["files"].as_u64().unwrap_or(0)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use oa_assets::detect::Console;
    use oa_assets::discover::{Root, Source};
    use std::sync::Arc;
    use std::sync::mpsc::channel;

    #[test]
    fn imports_console_art_with_progress_and_skips_panels() {
        let t = std::env::temp_dir().join(format!("oa-setup-import-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&t);
        let res = t.join("src/Resources");
        std::fs::create_dir_all(res.join("appearance")).unwrap();
        std::fs::write(res.join("appearance/Appearance.json"), "{}").unwrap();
        std::fs::write(res.join("big.png"), vec![7u8; 5000]).unwrap();
        let console = Console { dir: t.join("src"), resources: res, version: Some("1.3.1".into()) };
        let install = Install {
            root: Root { path: t.join("src"), source: Source::Given },
            console: Some(console),
            mixer_engine: None,
            powered_plugins: Vec::new(),
            vst3: Vec::new(),
            drivers: Vec::new(),
            uad_version: None,
            sdk_dll: None,
            firmware: Vec::new(),
            plugin_icons: None,
        };
        let plan = plan(Some(&install));
        assert_eq!(plan, [(Item::ConsoleArt, State::Waiting), (Item::PluginPanels, State::ComingSoon)]);

        let (tx, rx) = channel();
        let data = t.join("data");
        run(install, data.clone(), vec![Item::ConsoleArt, Item::PluginPanels], tx, Arc::new(|| {}));
        let msgs: Vec<Msg> = rx.iter().take_while(|m| *m != Msg::Finished).collect();
        assert!(matches!(msgs.first(), Some(Msg::State(Item::ConsoleArt, State::Copying { files: 0, .. }))));
        match msgs.last() {
            Some(Msg::State(Item::ConsoleArt, State::Done { files, bytes, dest })) => {
                assert_eq!((*files, *bytes), (2, 5002));
                assert_eq!(*dest, data.join(import::CONSOLE_DIR));
            }
            other => panic!("unexpected {other:?}"),
        }
        assert_eq!(previous(&data), Some((Some("1.3.1".into()), 2)));
        std::fs::remove_dir_all(&t).unwrap();
    }

    #[test]
    fn an_install_without_console_has_nothing_to_import() {
        let install = Install {
            root: Root { path: "/x".into(), source: Source::Given },
            console: None,
            mixer_engine: None,
            powered_plugins: Vec::new(),
            vst3: Vec::new(),
            drivers: Vec::new(),
            uad_version: None,
            sdk_dll: None,
            firmware: Vec::new(),
            plugin_icons: None,
        };
        assert!(matches!(plan(Some(&install))[0].1, State::NotInInstall(_)));
        assert_eq!(State::Copying { files: 1, bytes: 50, total_files: 2, total_bytes: 100 }.fraction(), Some(0.5));
    }
}
