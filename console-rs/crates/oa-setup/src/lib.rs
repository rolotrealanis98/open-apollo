//! open-apollo's setup wizard.
//!
//! open-apollo ships no Universal Audio software or art. The wizard finds the
//! user's own UAD install (a Windows partition, a Windows VM's disk image, a
//! Wine prefix, or a folder), imports what console-rs needs, extracts the DSP
//! programs and firmware the driver loads, installs the driver through
//! scripts/install.sh, and builds and starts the Console, showing each step.
//!
//! Everything but [`ui`] is plain logic with unit tests.

pub mod demo;
pub mod firmware;
pub mod importer;
pub mod installlog;
pub mod jobs;
pub mod options;
pub mod scan;
pub mod steps;
pub mod syscheck;
pub mod ui;
