//! httpdog's internals as a library crate, so that the binary (src/main.rs) and the fuzz targets (fuzz/)
//! run exactly the same code.

// httpdog reads text chosen by whoever sends requests to the site, so memory safety is not left to care:
// unsafe code does not compile in this crate, and forbid (unlike deny) cannot be switched off further down
#![forbid(unsafe_code)]

pub mod config;
pub mod engine;
pub mod format;
pub mod notify;
