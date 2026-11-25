//! Shared entry point for Chess Database API CLI names.

#[path = "main.rs"]
mod commands;

pub use commands::run;
