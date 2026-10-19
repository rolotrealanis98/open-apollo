//! open-apollo's setup wizard.
//!
//! open-apollo ships no Universal Audio software or art. The wizard finds the
//! user's own UAD install (a Windows partition, a Windows VM's disk image, a
//! Wine prefix, or a folder), then runs one privileged script
//! (scripts/setup-root.sh, one password prompt) that removes any earlier
//! open-apollo install, imports what console-rs needs, extracts the DSP
//! programs and firmware the driver loads and installs the driver; then it
//! builds and starts the Console as the user, showing each step.
//!
//! Everything but [`ui`] is plain logic with unit tests.

pub mod demo;
pub mod firmware;
pub mod importer;
pub mod installlog;
pub mod jobs;
pub mod options;
pub mod runlog;
pub mod scan;
pub mod setuprun;
pub mod steps;
pub mod syscheck;
pub mod ui;
