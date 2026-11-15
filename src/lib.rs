//! httpdog's internals as a library crate, so that the binary (src/main.rs) and the fuzz targets (fuzz/)
//! run exactly the same code.

pub mod config;
pub mod engine;
pub mod format;
pub mod notify;
