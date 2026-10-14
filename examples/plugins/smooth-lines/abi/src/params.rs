//! Parameter lookup for hosts without an allocator.
//!
//! The PhotoCraft host passes a plug-in its validated parameters as a flat JSON
//! object (plus an `_image` object). A `no_std` module without an allocator
//! can't run a JSON parser, so these find one top-level value by key with a
//! byte scan: enough for the plug-in's manifest parameters, which the host has
//! already validated and clamped.
//!
//! Recovery policy (SPEC §5): non-finite numbers (NaN/inf) fall back to the
//! caller-supplied default; out-of-range numbers clamp to `[min, max]`.
//!
//! Limitation: the scan is not nesting-aware, so a parameter name that also
//! occurs as a key inside `_image` (`x`, `y`, `width`, `height`, `overlap`,
//! `mode`, `depth`, `alpha`, `channels`, `canvasWidth`, `canvasHeight`) would
//! match whichever comes first. Don't name parameters that way.

/// The number stored under `"key"` in `json`, if there is one and it parses as a finite float.
pub fn number(json: &[u8], key: &str) -> Option<f32> {
    let value = value(json, key)?;
    let end = value
        .iter()
        .position(|b| !matches!(b, b'0'..=b'9' | b'-' | b'+' | b'.' | b'e' | b'E'))
        .unwrap_or(value.len());
    let text = core::str::from_utf8(value.get(..end)?).ok()?;
    text.parse::<f32>().ok().filter(|v| v.is_finite())
}

/// The number under `"key"`, or `fallback` when missing or non-finite.
pub fn number_or(json: &[u8], key: &str, fallback: f32) -> f32 {
    number(json, key).unwrap_or(fallback)
}

/// The number under `"key"` clamped to `[min, max]`, or `fallback` when
/// missing or non-finite.
pub fn clamped(json: &[u8], key: &str, min: f32, max: f32, fallback: f32) -> f32 {
    number_or(json, key, fallback).clamp(min, max)
}

/// The string stored under `"key"` in `json`, if there is one (no escape sequences).
pub fn string<'a>(json: &'a [u8], key: &str) -> Option<&'a str> {
    let value = value(json, key)?.strip_prefix(b"\"")?;
    let end = value.iter().position(|b| *b == b'"')?;
    let text = value.get(..end)?;
    if text.contains(&b'\\') {
        return None;
    }
    core::str::from_utf8(text).ok()
}

/// The boolean stored under `"key"` in `json`, if there is one.
pub fn boolean(json: &[u8], key: &str) -> Option<bool> {
    let value = value(json, key)?;
    if value.starts_with(b"true") {
        Some(true)
    } else if value.starts_with(b"false") {
        Some(false)
    } else {
        None
    }
}

/// The boolean under `"key"`, or `fallback` when missing or not a boolean.
pub fn boolean_or(json: &[u8], key: &str, fallback: bool) -> bool {
    boolean(json, key).unwrap_or(fallback)
}

/// `json` from the first byte of the value that follows `"key":`.
fn value<'a>(json: &'a [u8], key: &str) -> Option<&'a [u8]> {
    let mut from = 0;
    while let Some(at) = find_key(json, key, from) {
        let rest = skip_ws(json.get(at..)?);
        if let Some(after_colon) = rest.strip_prefix(b":") {
            return Some(skip_ws(after_colon));
        }
        from = at;
    }
    None
}

/// Offset just past the next `"key"` token at or after `from`.
fn find_key(json: &[u8], key: &str, from: usize) -> Option<usize> {
    let key = key.as_bytes();
    let needle_len = key.len().checked_add(2)?;
    let hay = json.get(from..)?;
    hay.windows(needle_len)
        .position(|w| {
            w.first() == Some(&b'"') && w.last() == Some(&b'"') && w.get(1..needle_len - 1) == Some(key)
        })
        .and_then(|i| from.checked_add(i)?.checked_add(needle_len))
}

fn skip_ws(s: &[u8]) -> &[u8] {
    let n = s
        .iter()
        .position(|b| !b.is_ascii_whitespace())
        .unwrap_or(s.len());
    s.get(n..).unwrap_or(&[])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_top_level_numbers() {
        let j = br#"{"amount": 30, "radius":2,"preserveEdges": 62.5, "_image": {"x": 0, "width": 1024}}"#;
        assert_eq!(number(j, "amount"), Some(30.0));
        assert_eq!(number(j, "radius"), Some(2.0));
        assert_eq!(number(j, "preserveEdges"), Some(62.5));
        assert_eq!(number(j, "width"), Some(1024.0));
        assert_eq!(number(j, "missing"), None);
    }

    #[test]
    fn accepts_float_spellings() {
        let j = br#"{"neg": -0.5e1, "plus": +2.0, "exp": 1E3, "dot": .5}"#;
        assert_eq!(number(j, "neg"), Some(-5.0));
        assert_eq!(number(j, "plus"), Some(2.0));
        assert_eq!(number(j, "exp"), Some(1000.0));
        assert_eq!(number(j, "dot"), Some(0.5));
    }

    #[test]
    fn falls_back_on_non_finite() {
        // NaN / inf spellings never surface as floats: callers get the default.
        assert_eq!(number(br#"{"a": NaN}"#, "a"), None);
        assert_eq!(number(br#"{"a": Infinity}"#, "a"), None);
        assert_eq!(number(br#"{"a": -Infinity}"#, "a"), None);
        assert_eq!(number(br#"{"a": 1e99999}"#, "a"), None);
        assert_eq!(number_or(br#"{"a": NaN}"#, "a", 30.0), 30.0);
        assert_eq!(clamped(br#"{"a": Infinity}"#, "a", 0.0, 100.0, 50.0), 50.0);
    }

    #[test]
    fn clamps_out_of_range() {
        assert_eq!(clamped(br#"{"r": 99}"#, "r", 0.0, 8.0, 2.0), 8.0);
        assert_eq!(clamped(br#"{"r": -3}"#, "r", 0.0, 8.0, 2.0), 0.0);
        assert_eq!(clamped(br#"{"r": 5}"#, "r", 0.0, 8.0, 2.0), 5.0);
        assert_eq!(clamped(br#"{}"#, "r", 0.0, 8.0, 2.0), 2.0);
    }

    #[test]
    fn finds_strings_and_booleans() {
        let j = br#"{"mode": "Standard", "smoothAlpha": true, "off":false, "n": 1}"#;
        assert_eq!(string(j, "mode"), Some("Standard"));
        assert_eq!(boolean(j, "smoothAlpha"), Some(true));
        assert_eq!(boolean(j, "off"), Some(false));
        assert!(boolean_or(j, "missing", true));
        assert_eq!(string(j, "n"), None);
        assert_eq!(boolean(j, "n"), None);
        assert_eq!(number(j, "mode"), None);
    }

    #[test]
    fn ignores_values_that_merely_contain_the_key() {
        let j = br#"{"mode": "amount", "amount": 40}"#;
        assert_eq!(number(j, "amount"), Some(40.0));
        let j = br#"{"mode": "amount"}"#;
        assert_eq!(number(j, "amount"), None);
        let j = br#"{"amounts": 3}"#;
        assert_eq!(number(j, "amount"), None);
    }

    #[test]
    fn rejects_garbage() {
        assert_eq!(number(br#"{"a": true}"#, "a"), None);
        assert_eq!(number(br#"{"a": "1"}"#, "a"), None);
        assert_eq!(number(b"", "a"), None);
        assert_eq!(number(b"\"a\"", "a"), None);
        assert_eq!(number(&[0xff, b'"', b'a', b'"'], "a"), None);
        assert_eq!(string(b"{\"s\": \"unterminated", "s"), None);
        assert_eq!(string(br#"{"s": "a\"b"}"#, "s"), None, "escapes are not supported");
        assert_eq!(boolean(br#"{"b": 1}"#, "b"), None);
    }

    #[test]
    fn tolerates_whitespace_layouts() {
        let j = b"{\n  \"amount\" \t : \n 30 \n}";
        assert_eq!(number(j, "amount"), Some(30.0));
    }
}
