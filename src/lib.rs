#![allow(dead_code)]

/// Find the largest valid UTF-8 char boundary at or before `byte_index`.
/// Polyfill for the unstable `str::floor_char_boundary`.
pub fn floor_char_boundary(s: &str, byte_index: usize) -> usize {
    if byte_index >= s.len() {
        return s.len();
    }
    let mut i = byte_index;
    while i > 0 && !s.is_char_boundary(i) {
        i -= 1;
    }
    i
}

#[cfg(test)]
mod lib_tests {
    use super::*;

    #[test]
    fn floor_char_boundary_ascii() {
        assert_eq!(floor_char_boundary("hello", 3), 3);
        assert_eq!(floor_char_boundary("hello", 10), 5); // past end → len
        assert_eq!(floor_char_boundary("hello", 0), 0);
    }

    #[test]
    fn floor_char_boundary_multibyte() {
        // '🦀' is 4 bytes: bytes 0..4
        let s = "🦀abc";
        assert_eq!(floor_char_boundary(s, 4), 4); // right after the emoji
        assert_eq!(floor_char_boundary(s, 3), 0); // mid-emoji → back to 0
        assert_eq!(floor_char_boundary(s, 2), 0);
        assert_eq!(floor_char_boundary(s, 1), 0);
    }

    #[test]
    fn floor_char_boundary_empty() {
        assert_eq!(floor_char_boundary("", 0), 0);
        assert_eq!(floor_char_boundary("", 5), 0);
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
