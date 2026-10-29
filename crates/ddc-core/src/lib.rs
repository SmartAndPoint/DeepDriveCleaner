//! DeepDriveCleaner core: scanning, rules, cleanup planning and execution.
//!
//! The core never talks to the terminal: no printing, no prompts. The CLI
//! (and later the Mac app) owns all user interaction. `sys::term` only reads
//! the terminal's width for the CLI, because `sys` is the one module allowed
//! `unsafe`.
#![deny(clippy::print_stdout, clippy::print_stderr)]

pub mod agents;
pub mod class;
pub mod disk;
pub mod env;
pub mod exec;
pub mod free;
pub mod heap;
pub mod model;
pub mod notify;
pub mod plan;
pub mod progress;
pub mod report;
pub mod rules;
pub mod scan;
pub mod settings;
pub mod stats;
pub mod store;
pub mod sys;
