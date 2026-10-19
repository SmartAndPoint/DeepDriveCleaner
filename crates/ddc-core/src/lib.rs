//! DeepDriveCleaner core: scanning, rules, cleanup planning and execution.
//!
//! The core never talks to the terminal: no printing, no prompts. The CLI
//! (and later the Mac app) owns all user interaction.
#![deny(clippy::print_stdout, clippy::print_stderr)]

pub mod class;
pub mod env;
pub mod exec;
pub mod model;
pub mod notify;
pub mod plan;
pub mod report;
pub mod rules;
pub mod scan;
pub mod settings;
pub mod store;
pub mod sys;
