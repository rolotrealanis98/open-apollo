//! The wizard's steps, their status, and which ones may be visited or are blocked.
//!
//! Pure state: the UI decides when a step starts and how it ends, this module
//! keeps the rules (forward only through finished steps, back anywhere already
//! reached, prerequisites, and resetting what depends on a changed choice).

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum StepId {
    Welcome,
    System,
    Find,
    Import,
    Firmware,
    Driver,
    Console,
    Done,
}

impl StepId {
    pub const ALL: [StepId; 8] = [
        StepId::Welcome,
        StepId::System,
        StepId::Find,
        StepId::Import,
        StepId::Firmware,
        StepId::Driver,
        StepId::Console,
        StepId::Done,
    ];

    pub fn index(self) -> usize {
        self as usize
    }

    /// Sidebar label.
    pub fn title(self) -> &'static str {
        match self {
            StepId::Welcome => "Welcome",
            StepId::System => "System check",
            StepId::Find => "Find your UAD install",
            StepId::Import => "Import assets",
            StepId::Firmware => "DSP programs & firmware",
            StepId::Driver => "Driver",
            StepId::Console => "Console",
            StepId::Done => "Done",
        }
    }

    /// Name used by `--step`.
    pub fn key(self) -> &'static str {
        match self {
            StepId::Welcome => "welcome",
            StepId::System => "system",
            StepId::Find => "find",
            StepId::Import => "import",
            StepId::Firmware => "firmware",
            StepId::Driver => "driver",
            StepId::Console => "console",
            StepId::Done => "done",
        }
    }

    pub fn parse(s: &str) -> Option<StepId> {
        Self::ALL.into_iter().find(|st| st.key().eq_ignore_ascii_case(s))
    }

    pub fn next(self) -> Option<StepId> {
        Self::ALL.get(self.index() + 1).copied()
    }

    pub fn prev(self) -> Option<StepId> {
        self.index().checked_sub(1).map(|i| Self::ALL[i])
    }

    /// Steps that must have succeeded before this one can run.
    pub fn prerequisites(self) -> &'static [StepId] {
        match self {
            StepId::Import | StepId::Firmware => &[StepId::Find],
            // The driver loads the DSP programs extracted from the user's install.
            StepId::Driver => &[StepId::Firmware],
            _ => &[],
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Status {
    Pending,
    Running,
    Done,
    Warning,
    Failed,
    Skipped,
}

impl Status {
    /// The step has ended, one way or another.
    pub fn is_finished(self) -> bool {
        matches!(self, Status::Done | Status::Warning | Status::Failed | Status::Skipped)
    }

    /// The step produced what later steps need.
    pub fn is_success(self) -> bool {
        matches!(self, Status::Done | Status::Warning)
    }

    /// How bad a status is, for picking the worst of several.
    fn severity(self) -> u8 {
        match self {
            Status::Pending => 0,
            Status::Skipped => 1,
            Status::Done => 2,
            Status::Running => 3,
            Status::Warning => 4,
            Status::Failed => 5,
        }
    }

    /// The worse of two statuses (a warning outranks success, a failure everything).
    pub fn worst(self, other: Status) -> Status {
        if other.severity() > self.severity() { other } else { self }
    }
}

#[derive(Clone, Debug)]
pub struct Steps {
    current: StepId,
    reached: StepId,
    status: [Status; StepId::ALL.len()],
}

impl Steps {
    /// Start at `start`; steps before it can be revisited.
    pub fn new(start: StepId) -> Self {
        Steps { current: start, reached: start, status: [Status::Pending; StepId::ALL.len()] }
    }

    pub fn current(&self) -> StepId {
        self.current
    }

    pub fn status(&self, step: StepId) -> Status {
        self.status[step.index()]
    }

    pub fn set(&mut self, step: StepId, status: Status) {
        self.status[step.index()] = status;
    }

    /// The first unmet prerequisite of `step`, if any.
    pub fn blocker(&self, step: StepId) -> Option<StepId> {
        step.prerequisites().iter().copied().find(|p| !self.status(*p).is_success())
    }

    /// Steps can be revisited from the sidebar once reached.
    pub fn can_visit(&self, step: StepId) -> bool {
        step <= self.reached
    }

    pub fn goto(&mut self, step: StepId) -> bool {
        if !self.can_visit(step) {
            return false;
        }
        self.current = step;
        true
    }

    /// Next is allowed once the current step has ended (a failure may be
    /// continued past; later steps show their own blockers).
    pub fn can_advance(&self) -> bool {
        self.current.next().is_some() && self.status(self.current).is_finished()
    }

    pub fn advance(&mut self) -> Option<StepId> {
        if !self.can_advance() {
            return None;
        }
        let next = self.current.next()?;
        self.current = next;
        self.reached = self.reached.max(next);
        Some(next)
    }

    pub fn back(&mut self) -> Option<StepId> {
        let prev = self.current.prev()?;
        self.current = prev;
        Some(prev)
    }

    /// `step` is being redone (e.g. a different install was picked): steps that
    /// depend on it, directly or not, go back to pending unless running.
    pub fn invalidate_dependents(&mut self, step: StepId) {
        for s in StepId::ALL {
            if s != step && self.depends_on(s, step) && self.status(s) != Status::Running {
                self.set(s, Status::Pending);
            }
        }
    }

    fn depends_on(&self, step: StepId, on: StepId) -> bool {
        step.prerequisites().iter().any(|p| *p == on || self.depends_on(*p, on))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn walks_forward_only_through_finished_steps() {
        let mut s = Steps::new(StepId::Welcome);
        assert!(!s.can_advance());
        assert!(!s.can_visit(StepId::System));
        s.set(StepId::Welcome, Status::Done);
        assert_eq!(s.advance(), Some(StepId::System));
        s.set(StepId::System, Status::Running);
        assert!(!s.can_advance(), "a running step holds Next");
        assert_eq!(s.advance(), None);
        s.set(StepId::System, Status::Warning);
        assert_eq!(s.advance(), Some(StepId::Find));
        assert!(s.can_visit(StepId::Welcome) && s.can_visit(StepId::Find));
        assert!(!s.can_visit(StepId::Import));
    }

    #[test]
    fn sidebar_goes_back_and_returns_to_reached_steps() {
        let mut s = Steps::new(StepId::Welcome);
        for st in [StepId::Welcome, StepId::System, StepId::Find] {
            s.set(st, Status::Done);
            s.advance();
        }
        assert_eq!(s.current(), StepId::Import);
        assert!(s.goto(StepId::System));
        assert!(s.goto(StepId::Import), "a reached step stays reachable after going back");
        assert!(!s.goto(StepId::Driver));
        assert_eq!(s.back(), Some(StepId::Find));
        assert_eq!(Steps::new(StepId::Welcome).back(), None);
    }

    #[test]
    fn a_failed_step_can_be_continued_past_but_blocks_dependents() {
        let mut s = Steps::new(StepId::Firmware);
        s.set(StepId::Find, Status::Done);
        assert_eq!(s.blocker(StepId::Driver), Some(StepId::Firmware));
        s.set(StepId::Firmware, Status::Failed);
        assert!(s.can_advance());
        assert_eq!(s.advance(), Some(StepId::Driver));
        assert_eq!(s.blocker(StepId::Driver), Some(StepId::Firmware));
        s.set(StepId::Firmware, Status::Warning);
        assert_eq!(s.blocker(StepId::Driver), None);
    }

    #[test]
    fn nothing_found_blocks_import_and_firmware() {
        let mut s = Steps::new(StepId::Find);
        s.set(StepId::Find, Status::Warning);
        assert_eq!(s.blocker(StepId::Import), None, "a warning still counts as found");
        s.set(StepId::Find, Status::Skipped);
        assert_eq!(s.blocker(StepId::Import), Some(StepId::Find));
        assert_eq!(s.blocker(StepId::Firmware), Some(StepId::Find));
        assert_eq!(s.blocker(StepId::Console), None);
    }

    #[test]
    fn changing_the_install_resets_what_depends_on_it() {
        let mut s = Steps::new(StepId::Done);
        for st in StepId::ALL {
            s.set(st, Status::Done);
        }
        s.set(StepId::Firmware, Status::Running);
        s.invalidate_dependents(StepId::Find);
        assert_eq!(s.status(StepId::Import), Status::Pending);
        assert_eq!(s.status(StepId::Firmware), Status::Running, "a running job is left alone");
        assert_eq!(s.status(StepId::Driver), Status::Pending, "transitively through firmware");
        assert_eq!(s.status(StepId::Find), Status::Done);
        assert_eq!(s.status(StepId::Console), Status::Done);
    }

    #[test]
    fn worst_status_and_parsing() {
        assert_eq!(Status::Done.worst(Status::Warning), Status::Warning);
        assert_eq!(Status::Failed.worst(Status::Warning), Status::Failed);
        assert_eq!(Status::Pending.worst(Status::Done), Status::Done);
        assert_eq!(StepId::parse("Firmware"), Some(StepId::Firmware));
        assert_eq!(StepId::parse("nope"), None);
        assert_eq!(StepId::Done.next(), None);
        assert!(StepId::ALL.iter().enumerate().all(|(i, s)| s.index() == i));
    }
}
