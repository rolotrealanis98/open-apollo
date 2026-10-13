//! Running an installer script (or replaying a fake one in `--demo`) on
//! background threads, streaming its output line by line.

use std::io::{BufRead, BufReader, Read};
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::sync::mpsc::{Receiver, Sender, channel};
use std::thread;
use std::time::Duration;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum JobMsg {
    Line(String),
    /// The process ended: its exit code, `None` when killed by a signal.
    Exit(Option<i32>),
    /// It couldn't be started at all.
    SpawnError(String),
}

/// Called from worker threads after each message, to wake the UI.
pub type Waker = Arc<dyn Fn() + Send + Sync>;

pub struct Job {
    rx: Receiver<JobMsg>,
}

impl Job {
    /// Start `cmd` with stdin closed and stdout/stderr captured.
    pub fn spawn(mut cmd: Command, wake: Waker) -> Job {
        let (tx, rx) = channel();
        cmd.stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped());
        match cmd.spawn() {
            Err(e) => {
                let _ = tx.send(JobMsg::SpawnError(format!("{}: {e}", cmd.get_program().to_string_lossy())));
                wake();
            }
            Ok(mut child) => {
                let readers: Vec<_> = [
                    child.stdout.take().map(|s| Box::new(s) as Box<dyn Read + Send>),
                    child.stderr.take().map(|s| Box::new(s) as Box<dyn Read + Send>),
                ]
                .into_iter()
                .flatten()
                .map(|r| {
                    let (tx, wake) = (tx.clone(), wake.clone());
                    thread::spawn(move || pump(r, &tx, &wake))
                })
                .collect();
                thread::spawn(move || {
                    for r in readers {
                        let _ = r.join();
                    }
                    let code = child.wait().ok().and_then(|s| s.code());
                    let _ = tx.send(JobMsg::Exit(code));
                    wake();
                });
            }
        }
        Job { rx }
    }

    /// Replay `script` (delay before each line, line) and exit with `exit`.
    pub fn replay(script: Vec<(Duration, String)>, exit: i32, wake: Waker) -> Job {
        let (tx, rx) = channel();
        thread::spawn(move || {
            for (delay, line) in script {
                thread::sleep(delay);
                if tx.send(JobMsg::Line(line)).is_err() {
                    return;
                }
                wake();
            }
            let _ = tx.send(JobMsg::Exit(Some(exit)));
            wake();
        });
        Job { rx }
    }

    /// Everything received since the last call.
    pub fn poll(&self) -> Vec<JobMsg> {
        self.rx.try_iter().collect()
    }
}

/// Forward lines from `r`, splitting on `\n` and `\r` (progress output redraws with `\r`).
fn pump(r: Box<dyn Read + Send>, tx: &Sender<JobMsg>, wake: &Waker) {
    let mut r = BufReader::new(r);
    let mut buf = Vec::new();
    loop {
        buf.clear();
        match r.read_until(b'\n', &mut buf) {
            Ok(0) | Err(_) => return,
            Ok(_) => {
                let text = String::from_utf8_lossy(&buf);
                for part in text.trim_end_matches(['\n', '\r']).split('\r') {
                    if tx.send(JobMsg::Line(part.to_string())).is_err() {
                        return;
                    }
                }
                wake();
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn collect(job: &Job) -> Vec<JobMsg> {
        let mut out = Vec::new();
        for _ in 0..500 {
            out.extend(job.poll());
            if matches!(out.last(), Some(JobMsg::Exit(_) | JobMsg::SpawnError(_))) {
                return out;
            }
            thread::sleep(Duration::from_millis(10));
        }
        panic!("job never finished: {out:?}");
    }

    #[test]
    fn streams_stdout_and_stderr_then_exit() {
        let mut cmd = Command::new("sh");
        cmd.args(["-c", "echo one; echo two >&2; printf 'a\\rb\\n'; exit 3"]);
        let msgs = collect(&Job::spawn(cmd, Arc::new(|| {})));
        let lines: Vec<_> =
            msgs.iter().filter_map(|m| if let JobMsg::Line(l) = m { Some(l.as_str()) } else { None }).collect();
        for want in ["one", "two", "a", "b"] {
            assert!(lines.contains(&want), "{want} in {lines:?}");
        }
        assert_eq!(msgs.last(), Some(&JobMsg::Exit(Some(3))));
    }

    #[test]
    fn reports_a_missing_program() {
        let msgs = collect(&Job::spawn(Command::new("/nonexistent/oa-setup-test"), Arc::new(|| {})));
        assert!(matches!(&msgs[..], [JobMsg::SpawnError(_)]));
    }

    #[test]
    fn replays_a_demo_script() {
        let job = Job::replay(vec![(Duration::ZERO, "x".into()), (Duration::ZERO, "y".into())], 0, Arc::new(|| {}));
        assert_eq!(collect(&job), [JobMsg::Line("x".into()), JobMsg::Line("y".into()), JobMsg::Exit(Some(0))]);
    }
}
