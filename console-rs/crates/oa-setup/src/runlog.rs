//! The setup log: one file per install run that the user can attach to a bug
//! report, in `~/.local/state/open-apollo/setup-<UTC date>-<time>.log`.
//!
//! The wizard creates the file and writes its own lines (what was chosen, how
//! each part ended); `scripts/setup-root.sh --log FILE` and
//! `scripts/install-uaconsole.sh` (with `OA_INSTALL_LOG=FILE`) append their
//! whole output through `scripts/oa_redact.py`. Every line the wizard writes
//! goes through [`Redactor`], the same rules as oa_redact.py: no user name,
//! home folder, host name, machine id, serial numbers or MAC addresses.

use std::fs::{File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use crate::installlog::strip_ansi;

/// Takes the user's and the computer's identity out of a line (scripts/oa_redact.py's rules).
#[derive(Clone, Debug, Default)]
pub struct Redactor {
    home: Option<String>,
    machine_id: Option<String>,
    hosts: Vec<String>,
    user: Option<String>,
}

impl Redactor {
    /// For `user` with `home` on host `host` with `machine_id`; empty values are ignored.
    pub fn new(user: &str, home: &str, host: &str, machine_id: &str) -> Self {
        let some = |s: &str, min: usize| (s.len() >= min).then(|| s.to_string());
        let mut hosts: Vec<String> = [host, host.split('.').next().unwrap_or("")]
            .into_iter()
            .filter(|h| h.len() >= 2 && *h != "localhost")
            .map(str::to_string)
            .collect();
        hosts.sort_by_key(|h| std::cmp::Reverse(h.len()));
        hosts.dedup();
        Redactor {
            home: some(home, 2).filter(|h| h != "/"),
            machine_id: some(machine_id, 1),
            hosts,
            user: some(user, 2).filter(|u| u != "root"),
        }
    }

    /// This computer and the user running the wizard.
    pub fn from_env() -> Self {
        let read = |p: &str| std::fs::read_to_string(p).map(|s| s.trim().to_string()).unwrap_or_default();
        let user = std::env::var("USER").or_else(|_| std::env::var("LOGNAME")).unwrap_or_default();
        let home = std::env::var("HOME").unwrap_or_default();
        let mut host = read("/proc/sys/kernel/hostname");
        if host.is_empty() {
            host = read("/etc/hostname");
        }
        Redactor::new(&user, &home, &host, &read("/etc/machine-id"))
    }

    pub fn redact(&self, line: &str) -> String {
        let mut s = redact_serials(line);
        s = redact_macs(&s);
        if let Some(h) = &self.home {
            s = s.replace(h.as_str(), "<home>");
        }
        if let Some(m) = &self.machine_id {
            s = s.replace(m.as_str(), "<machine-id>");
        }
        for h in &self.hosts {
            s = replace_word(&s, h, "<host>");
        }
        if let Some(u) = &self.user {
            s = replace_word(&s, u, "<user>");
        }
        s
    }
}

fn is_word(c: Option<char>) -> bool {
    c.is_some_and(|c| c.is_ascii_alphanumeric())
}

/// `word` where it isn't part of a longer run of letters and digits.
fn replace_word(s: &str, word: &str, with: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(i) = rest.find(word) {
        let before = rest[..i].chars().next_back().or_else(|| out.chars().next_back());
        let after = rest[i + word.len()..].chars().next();
        out.push_str(&rest[..i]);
        if is_word(before) || is_word(after) {
            out.push_str(word);
        } else {
            out.push_str(with);
        }
        rest = &rest[i + word.len()..];
    }
    out.push_str(rest);
    out
}

/// "serial: 2000…", "serial number 12AB34", "S/N=XYZ123" -> the value becomes `<redacted>`
/// (oa_redact.SERIAL_RE: a keyword, optional `:`/`=`/`#`, then 6+ of `[0-9A-Za-z-]`
/// starting with a letter or digit).
fn redact_serials(s: &str) -> String {
    let lower = s.to_ascii_lowercase();
    let lower = lower.as_bytes();
    let b = s.as_bytes();
    // The value after the keyword ending at `j`: (start, end) when it is long enough.
    let value = |mut j: usize| {
        while j < b.len() && b[j].is_ascii_whitespace() {
            j += 1;
        }
        if j < b.len() && matches!(b[j], b':' | b'=' | b'#') {
            j += 1;
        }
        while j < b.len() && b[j].is_ascii_whitespace() {
            j += 1;
        }
        let start = j;
        if j < b.len() && b[j].is_ascii_alphanumeric() {
            while j < b.len() && (b[j].is_ascii_alphanumeric() || b[j] == b'-') {
                j += 1;
            }
        }
        (j - start >= 6).then_some((start, j))
    };
    let mut out = String::with_capacity(s.len());
    let mut copied = 0;
    let mut i = 0;
    while i < b.len() {
        // Like the regex: "serial number" first, else "serial" alone.
        let mut ends = Vec::new();
        // Byte slices: `i` can be inside a multi-byte character.
        if lower[i..].starts_with(b"serial") {
            for sep in ["number", " number", "_number"] {
                if lower[i + 6..].starts_with(sep.as_bytes()) {
                    ends.push(i + 6 + sep.len());
                }
            }
            ends.push(i + 6);
        } else if lower[i..].starts_with(b"s/n") {
            ends.push(i + 3);
        }
        match ends.into_iter().find_map(value) {
            Some((start, end)) => {
                out.push_str(&s[copied..start]);
                out.push_str("<redacted>");
                copied = end;
                i = end;
            }
            None => i += 1,
        }
    }
    out.push_str(&s[copied..]);
    out
}

/// `aa:bb:cc:dd:ee:ff` (also with `-` or `_`) -> `<mac>`.
fn redact_macs(s: &str) -> String {
    let b = s.as_bytes();
    let hex_or_colon = |i: usize| i < b.len() && (b[i].is_ascii_hexdigit() || b[i] == b':');
    let is_mac = |i: usize| {
        i + 17 <= b.len()
            && (0..6).all(|k| b[i + 3 * k].is_ascii_hexdigit() && b[i + 3 * k + 1].is_ascii_hexdigit())
            && (0..5).all(|k| matches!(b[i + 3 * k + 2], b':' | b'-' | b'_'))
    };
    let mut out = String::with_capacity(s.len());
    let mut copied = 0;
    let mut i = 0;
    while i < b.len() {
        if (i == 0 || !hex_or_colon(i - 1)) && is_mac(i) && !hex_or_colon(i + 17) {
            out.push_str(&s[copied..i]);
            out.push_str("<mac>");
            i += 17;
            copied = i;
        } else {
            i += 1;
        }
    }
    out.push_str(&s[copied..]);
    out
}

/// One log line as oa_redact.py writes it: no colour codes or control characters, at most 4000 characters.
pub fn clean(line: &str) -> String {
    let s = strip_ansi(line);
    let s = s.trim_end_matches(['\n', '\r']);
    let s = s.rsplit('\r').next().unwrap_or("");
    let mut s: String = s.chars().filter(|c| !matches!(*c, '\0'..='\x08' | '\x0b'..='\x1f' | '\x7f')).collect();
    if let Some((i, _)) = s.char_indices().nth(4000) {
        s.truncate(i);
        s.push_str(" …(cut)");
    }
    s
}

/// (year, month, day, hour, minute, second) in UTC.
fn utc(t: SystemTime) -> (i64, u32, u32, u32, u32, u32) {
    let secs = t.duration_since(UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or(0);
    let (days, rem) = (secs.div_euclid(86_400), secs.rem_euclid(86_400));
    // Howard Hinnant's days-to-civil
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    let y = yoe + era * 400 + i64::from(m <= 2);
    (y, m, d, (rem / 3600) as u32, (rem % 3600 / 60) as u32, (rem % 60) as u32)
}

/// `2026-10-19T15:04:05Z`
pub fn iso_utc(t: SystemTime) -> String {
    let (y, mo, d, h, mi, s) = utc(t);
    format!("{y:04}-{mo:02}-{d:02}T{h:02}:{mi:02}:{s:02}Z")
}

/// Where the logs go: `$XDG_STATE_HOME/open-apollo`, else `~/.local/state/open-apollo`.
pub fn log_dir() -> PathBuf {
    match std::env::var_os("XDG_STATE_HOME").filter(|v| !v.is_empty()) {
        Some(state) => PathBuf::from(state).join("open-apollo"),
        None => PathBuf::from(std::env::var_os("HOME").unwrap_or_default()).join(".local/state").join("open-apollo"),
    }
}

/// The setup log of one run.
#[derive(Debug)]
pub struct RunLog {
    path: PathBuf,
    file: File,
    redactor: Redactor,
}

impl RunLog {
    /// A new `setup-<date>-<time>.log` in `dir` (made if missing), only the user may read it.
    pub fn create(dir: &Path, now: SystemTime, redactor: Redactor) -> io::Result<RunLog> {
        std::fs::create_dir_all(dir)?;
        let (y, mo, d, h, mi, s) = utc(now);
        let stem = format!("setup-{y:04}{mo:02}{d:02}-{h:02}{mi:02}{s:02}");
        let mut opts = OpenOptions::new();
        opts.append(true).create_new(true);
        #[cfg(unix)]
        std::os::unix::fs::OpenOptionsExt::mode(&mut opts, 0o600);
        for n in 0..100 {
            let name = if n == 0 { format!("{stem}.log") } else { format!("{stem}-{n}.log") };
            let path = dir.join(name);
            match opts.open(&path) {
                Ok(file) => return Ok(RunLog { path, file, redactor }),
                Err(e) if e.kind() == io::ErrorKind::AlreadyExists => continue,
                Err(e) => return Err(e),
            }
        }
        Err(io::Error::new(io::ErrorKind::AlreadyExists, "no free log file name"))
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Append one line, cleaned and redacted, with the time of day (UTC). A
    /// failed write is ignored: the log never stops the install.
    pub fn line(&mut self, text: &str) {
        let (.., h, mi, s) = utc(SystemTime::now());
        let _ = writeln!(self.file, "{h:02}:{mi:02}:{s:02} {}", self.redactor.redact(&clean(text)));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    fn r() -> Redactor {
        Redactor::new("yourname", "/home/yourname", "studio.lan", "0123456789abcdef0123456789abcdef")
    }

    /// The same cases as mixer-engine/tests/test_oa_redact.py (tests/fixtures/redact-cases.tsv).
    #[test]
    fn redacts_like_oa_redact_py() {
        let cases = include_str!("../../../../tests/fixtures/redact-cases.tsv");
        let mut n = 0;
        for row in cases.lines().filter(|l| !l.is_empty() && !l.starts_with('#')) {
            let (input, want) = row.split_once('\t').expect("input<TAB>expected");
            assert_eq!(r().redact(&clean(input)), want, "for {input:?}");
            n += 1;
        }
        assert!(n >= 10);
    }

    #[test]
    fn short_or_missing_identity_is_ignored() {
        let r = Redactor::new("", "/", "localhost", "");
        assert_eq!(r.redact("root@localhost:/ ok"), "root@localhost:/ ok");
        let r = Redactor::new("root", "/root", "x", "");
        assert_eq!(r.redact("root in /root/x"), "root in <home>/x");
    }

    #[test]
    fn cleans_colour_and_control_characters() {
        assert_eq!(clean("\x1b[0;32m[ OK ]\x1b[0m  done\r\n"), "[ OK ]  done");
        assert_eq!(clean("10%\r50%\r100%"), "100%");
        assert_eq!(clean("a\x07b\tc"), "ab\tc");
        assert!(clean(&"x".repeat(5000)).ends_with(" …(cut)"));
    }

    #[test]
    fn dates_are_utc() {
        assert_eq!(iso_utc(UNIX_EPOCH), "1970-01-01T00:00:00Z");
        assert_eq!(iso_utc(UNIX_EPOCH + Duration::from_secs(1_792_422_245)), "2026-10-19T15:04:05Z");
        assert_eq!(iso_utc(UNIX_EPOCH + Duration::from_secs(951_782_400)), "2000-02-29T00:00:00Z");
    }

    #[test]
    fn writes_a_new_private_file_per_run() {
        let dir = std::env::temp_dir().join(format!("oa-runlog-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let at = UNIX_EPOCH + Duration::from_secs(1_792_422_245);
        let mut a = RunLog::create(&dir, at, r()).unwrap();
        let b = RunLog::create(&dir, at, r()).unwrap();
        assert_eq!(a.path().file_name().unwrap(), "setup-20261019-150405.log");
        assert_eq!(b.path().file_name().unwrap(), "setup-20261019-150405-1.log");
        a.line("\x1b[1m── Installing ──\x1b[0m from /home/yourname/win on studio, S/N: 2000123456");
        let text = std::fs::read_to_string(a.path()).unwrap();
        assert!(text.ends_with(" ── Installing ── from <home>/win on <host>, S/N: <redacted>\n"), "{text}");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(std::fs::metadata(a.path()).unwrap().permissions().mode() & 0o777, 0o600);
        }
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
