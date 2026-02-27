//! Context management for reducing LLM API token usage.
//!
//! This module provides two subsystems:
//! - **Compressor**: Replaces already-seen tool results with compact summaries
//! - **Cache**: Caches tool results to avoid re-executing identical operations

pub mod cache;
pub mod compressor;
