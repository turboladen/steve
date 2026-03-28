use std::{
    io::{Read, Write},
    sync::mpsc,
    time::Duration,
};

use serde::{Deserialize, Serialize};

use super::theme::Theme;

/// User preference for theme selection, stored in config.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum ThemePreference {
    /// Auto-detect from terminal background color (default).
    #[default]
    Auto,
    /// Force dark theme.
    Dark,
    /// Force light theme.
    Light,
}

/// Result of terminal background detection.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DetectedBackground {
    Dark,
    Light,
}

/// Send OSC 11 query and read the terminal's background color response.
///
/// Must be called while in raw mode, before entering the alternate screen.
/// Returns `Dark` on timeout, I/O error, or if the terminal doesn't respond.
pub fn detect_background() -> DetectedBackground {
    // Send the OSC 11 query to stdout
    let mut stdout = std::io::stdout();
    if stdout.write_all(b"\x1b]11;?\x07").is_err() || stdout.flush().is_err() {
        return DetectedBackground::Dark;
    }

    // Spawn a thread to do blocking stdin reads (terminals respond on stdin)
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let mut buf = Vec::with_capacity(64);
        let stdin = std::io::stdin();
        let mut handle = stdin.lock();
        let mut byte = [0u8; 1];

        // Read byte-by-byte until we see BEL (\x07) or ST (\x1b\\)
        loop {
            match handle.read(&mut byte) {
                Ok(0) => break, // EOF
                Ok(_) => {
                    buf.push(byte[0]);
                    // BEL terminator
                    if byte[0] == 0x07 {
                        break;
                    }
                    // ST terminator (\x1b\\)
                    if buf.len() >= 2 && buf[buf.len() - 2] == 0x1b && byte[0] == b'\\' {
                        break;
                    }
                    // Safety: don't read forever
                    if buf.len() > 128 {
                        break;
                    }
                }
                Err(_) => break,
            }
        }

        let _ = tx.send(String::from_utf8_lossy(&buf).into_owned());
    });

    // Wait up to 100ms for the response
    match rx.recv_timeout(Duration::from_millis(100)) {
        Ok(response) => {
            if let Some((r, g, b)) = parse_osc11_response(&response) {
                if perceived_luminance(r, g, b) > 0.5 {
                    DetectedBackground::Light
                } else {
                    DetectedBackground::Dark
                }
            } else {
                DetectedBackground::Dark
            }
        }
        Err(_) => DetectedBackground::Dark,
    }
}

/// Parse an OSC 11 response to extract RGB values.
///
/// Handles both 2-digit (8-bit) and 4-digit (16-bit) hex components.
/// Response format: `\x1b]11;rgb:RRRR/GGGG/BBBB\x07` (or ST terminator).
fn parse_osc11_response(response: &str) -> Option<(u8, u8, u8)> {
    // Find the "rgb:" prefix
    let rgb_start = response.find("rgb:")?;
    let rgb_part = &response[rgb_start + 4..];

    // Strip any trailing terminators (BEL, ST, escape sequences)
    let rgb_clean = rgb_part
        .trim_end_matches('\x07')
        .trim_end_matches('\\')
        .trim_end_matches('\x1b')
        .trim_end_matches('\x07');

    let components: Vec<&str> = rgb_clean.split('/').collect();
    if components.len() != 3 {
        return None;
    }

    let r = parse_hex_component(components[0])?;
    let g = parse_hex_component(components[1])?;
    let b = parse_hex_component(components[2])?;

    Some((r, g, b))
}

/// Parse a hex color component, handling both 2-digit and 4-digit formats.
/// For 4-digit (16-bit), takes the high byte.
fn parse_hex_component(s: &str) -> Option<u8> {
    match s.len() {
        2 => u8::from_str_radix(s, 16).ok(),
        4 => {
            // 16-bit value — take the high byte (first 2 digits)
            u8::from_str_radix(&s[..2], 16).ok()
        }
        _ => None,
    }
}

/// Compute approximate perceived luminance using linear channel weighting.
/// Skips sRGB gamma expansion for simplicity — sufficient for binary light/dark
/// classification of typical terminal backgrounds.
/// Returns a value from 0.0 (black) to 1.0 (white).
fn perceived_luminance(r: u8, g: u8, b: u8) -> f64 {
    let r_norm = r as f64 / 255.0;
    let g_norm = g as f64 / 255.0;
    let b_norm = b as f64 / 255.0;
    0.2126 * r_norm + 0.7152 * g_norm + 0.0722 * b_norm
}

/// Resolve the final theme based on user preference and detected background.
pub fn resolve_theme(pref: ThemePreference, detected: DetectedBackground) -> Theme {
    match pref {
        ThemePreference::Dark => Theme::dark(),
        ThemePreference::Light => Theme::light(),
        ThemePreference::Auto => match detected {
            DetectedBackground::Dark => Theme::dark(),
            DetectedBackground::Light => Theme::light(),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // -- parse_osc11_response tests --

    #[test]
    fn parse_4digit_dark_background() {
        // Typical dark terminal (e.g., near-black)
        let response = "\x1b]11;rgb:1c1c/1c1c/1c1c\x07";
        let (r, g, b) = parse_osc11_response(response).unwrap();
        assert_eq!((r, g, b), (0x1c, 0x1c, 0x1c));
    }

    #[test]
    fn parse_4digit_white_background() {
        let response = "\x1b]11;rgb:ffff/ffff/ffff\x07";
        let (r, g, b) = parse_osc11_response(response).unwrap();
        assert_eq!((r, g, b), (0xff, 0xff, 0xff));
    }

    #[test]
    fn parse_2digit_hex() {
        let response = "\x1b]11;rgb:00/00/00\x07";
        let (r, g, b) = parse_osc11_response(response).unwrap();
        assert_eq!((r, g, b), (0, 0, 0));
    }

    #[test]
    fn parse_st_terminator() {
        // ST = ESC + backslash
        let response = "\x1b]11;rgb:ff/ff/ff\x1b\\";
        let (r, g, b) = parse_osc11_response(response).unwrap();
        assert_eq!((r, g, b), (0xff, 0xff, 0xff));
    }

    #[test]
    fn parse_solarized_dark() {
        // Solarized Dark base03: #002b36 → rgb:0000/2b2b/3636
        let response = "\x1b]11;rgb:0000/2b2b/3636\x07";
        let (r, g, b) = parse_osc11_response(response).unwrap();
        assert_eq!((r, g, b), (0x00, 0x2b, 0x36));
    }

    #[test]
    fn parse_solarized_light() {
        // Solarized Light base3: #fdf6e3 → rgb:fdfd/f6f6/e3e3
        let response = "\x1b]11;rgb:fdfd/f6f6/e3e3\x07";
        let (r, g, b) = parse_osc11_response(response).unwrap();
        assert_eq!((r, g, b), (0xfd, 0xf6, 0xe3));
    }

    #[test]
    fn parse_no_rgb_prefix() {
        let response = "\x1b]11;foo:bar\x07";
        assert!(parse_osc11_response(response).is_none());
    }

    #[test]
    fn parse_wrong_component_count() {
        let response = "\x1b]11;rgb:ff/ff\x07";
        assert!(parse_osc11_response(response).is_none());
    }

    #[test]
    fn parse_empty_string() {
        assert!(parse_osc11_response("").is_none());
    }

    // -- perceived_luminance tests --

    #[test]
    fn luminance_black_is_zero() {
        assert!((perceived_luminance(0, 0, 0) - 0.0).abs() < f64::EPSILON);
    }

    #[test]
    fn luminance_white_is_one() {
        assert!((perceived_luminance(255, 255, 255) - 1.0).abs() < 0.01);
    }

    #[test]
    fn luminance_dark_gray_below_threshold() {
        // RGB(50, 50, 50) should be dark
        assert!(perceived_luminance(50, 50, 50) < 0.5);
    }

    #[test]
    fn luminance_light_gray_above_threshold() {
        // RGB(200, 200, 200) should be light
        assert!(perceived_luminance(200, 200, 200) > 0.5);
    }

    #[test]
    fn luminance_solarized_dark_is_dark() {
        // Solarized Dark base03: #002b36
        assert!(perceived_luminance(0x00, 0x2b, 0x36) < 0.5);
    }

    #[test]
    fn luminance_solarized_light_is_light() {
        // Solarized Light base3: #fdf6e3
        assert!(perceived_luminance(0xfd, 0xf6, 0xe3) > 0.5);
    }

    // -- ThemePreference serde tests --

    #[test]
    fn theme_preference_default_is_auto() {
        assert_eq!(ThemePreference::default(), ThemePreference::Auto);
    }

    #[test]
    fn theme_preference_deserialize_auto() {
        let pref: ThemePreference = serde_json::from_str(r#""auto""#).unwrap();
        assert_eq!(pref, ThemePreference::Auto);
    }

    #[test]
    fn theme_preference_deserialize_dark() {
        let pref: ThemePreference = serde_json::from_str(r#""dark""#).unwrap();
        assert_eq!(pref, ThemePreference::Dark);
    }

    #[test]
    fn theme_preference_deserialize_light() {
        let pref: ThemePreference = serde_json::from_str(r#""light""#).unwrap();
        assert_eq!(pref, ThemePreference::Light);
    }

    // -- resolve_theme tests --

    #[test]
    fn theme_preference_serialize_roundtrip() {
        for (pref, expected) in [
            (ThemePreference::Auto, "\"auto\""),
            (ThemePreference::Dark, "\"dark\""),
            (ThemePreference::Light, "\"light\""),
        ] {
            let json = serde_json::to_string(&pref).unwrap();
            assert_eq!(json, expected);
            let back: ThemePreference = serde_json::from_str(&json).unwrap();
            assert_eq!(back, pref);
        }
    }

    #[test]
    fn theme_preference_rejects_invalid() {
        assert!(serde_json::from_str::<ThemePreference>(r#""purple""#).is_err());
        assert!(serde_json::from_str::<ThemePreference>(r#""""#).is_err());
    }

    // -- resolve_theme tests --

    #[test]
    fn resolve_auto_dark_gives_dark() {
        let theme = resolve_theme(ThemePreference::Auto, DetectedBackground::Dark);
        assert_eq!(theme, Theme::dark());
    }

    #[test]
    fn resolve_auto_light_gives_light() {
        let theme = resolve_theme(ThemePreference::Auto, DetectedBackground::Light);
        assert_eq!(theme, Theme::light());
    }

    #[test]
    fn resolve_explicit_dark_ignores_detection() {
        let theme = resolve_theme(ThemePreference::Dark, DetectedBackground::Light);
        assert_eq!(theme, Theme::dark());
    }

    #[test]
    fn resolve_explicit_light_ignores_detection() {
        let theme = resolve_theme(ThemePreference::Light, DetectedBackground::Dark);
        assert_eq!(theme, Theme::light());
    }
}
