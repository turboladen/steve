#![allow(dead_code)]
#![warn(clippy::cargo)]
#![allow(clippy::multiple_crate_versions)]

#[cfg(test)]
mod lib_tests {
    use super::*;

    #[test]
    fn truncate_chars_short_passthrough() {
        assert_eq!(truncate_chars("hello", 10), "hello");
    }

    #[test]
    fn truncate_chars_exact_length() {
        assert_eq!(truncate_chars("hello", 5), "hello");
    }

    #[test]
    fn truncate_chars_truncates() {
        assert_eq!(truncate_chars("hello world", 8), "hello...");
    }

    #[test]
    fn truncate_chars_unicode() {
        let s = "🦀".repeat(10);
        let result = truncate_chars(&s, 7);
        assert_eq!(result.chars().count(), 7);
        assert!(result.ends_with("..."));
    }
}

/// Truncate a string to at most `max` Unicode scalar values (chars).
/// Appends "..." when truncated (requires `max >= 4`).
/// For `max < 4`, truncates without ellipsis to always enforce the limit.
/// Note: counts `char`s, not grapheme clusters or display width.
pub fn truncate_chars(s: &str, max: usize) -> String {
    let len = s.chars().count();
    if len <= max {
        return s.to_string();
    }
    if max < 4 {
        return s.chars().take(max).collect();
    }
    let truncated: String = s.chars().take(max - 3).collect();
    format!("{truncated}...")
}

/// Extension trait for consistent date/time formatting across the codebase.
pub trait DateTimeExt {
    /// Format as `"2026-03-28 14:30"` — for UI display of timestamps.
    fn display_short(&self) -> String;
    /// Format as `"2026-03-28"` — date only.
    fn display_date(&self) -> String;
    /// Format as `"2026-03-28 14:30:00 UTC"` — for export/debug output.
    fn display_full_utc(&self) -> String;
}

impl<Tz: chrono::TimeZone> DateTimeExt for chrono::DateTime<Tz>
where
    Tz::Offset: std::fmt::Display,
{
    fn display_short(&self) -> String {
        self.format("%Y-%m-%d %H:%M").to_string()
    }

    fn display_date(&self) -> String {
        self.format("%Y-%m-%d").to_string()
    }

    fn display_full_utc(&self) -> String {
        self.with_timezone(&chrono::Utc)
            .format("%Y-%m-%d %H:%M:%S UTC")
            .to_string()
    }
}

pub mod app;
pub mod cli;
pub mod command;
pub mod config;
pub mod context;
pub mod data;
pub mod diagnostics;
pub mod event;
pub mod export;
pub mod file_ref;
pub mod lsp;
pub mod mcp;
pub mod permission;
pub mod project;
pub mod provider;
pub mod session;
pub mod storage;
pub mod stream;
pub mod task;
pub mod tool;
pub mod ui;
pub mod usage;
