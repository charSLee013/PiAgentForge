//! Pi Model Catalog — model data code generation.
//!
//! This crate contains the model catalog data and code generation logic.

pub mod models;

/// Get the catalog version.
pub const CATALOG_VERSION: &str = env!("CARGO_PKG_VERSION");
