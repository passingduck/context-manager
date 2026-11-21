//! Canonical JSON as Claude Code hashes it (session spec, section 3.2): no whitespace, object
//! keys sorted by UTF-16 code units, strings and numbers as JavaScript's `JSON.stringify` writes
//! them, and optionally every `cache_control` key left out at any depth.

use serde_json::{Map, Number, Value};
use sha2::{Digest, Sha256};

/// A SHA-256 digest, as the record names it in lowercase hex.
pub(super) type Hash = [u8; 32];

pub(super) fn sha256(bytes: &[u8]) -> Hash {
    Sha256::digest(bytes).into()
}

/// The 32 bytes of a lowercase or uppercase hex digest, or `None` for anything else.
pub(super) fn parse_hash(hex: &str) -> Option<Hash> {
    let hex = hex.as_bytes();
    if hex.len() != 64 {
        return None;
    }
    let digit = |c: u8| match c {
        b'0'..=b'9' => Some(c - b'0'),
        b'a'..=b'f' => Some(c - b'a' + 10),
        b'A'..=b'F' => Some(c - b'A' + 10),
        _ => None,
    };
    let mut out = [0u8; 32];
    for (i, pair) in hex.chunks(2).enumerate() {
        out[i] = digit(pair[0])? << 4 | digit(pair[1])?;
    }
    Some(out)
}

/// Appends the canonical text of `value`.
pub(super) fn write(out: &mut Vec<u8>, value: &Value, omit_cache_control: bool) {
    match value {
        Value::Null => out.extend_from_slice(b"null"),
        Value::Bool(true) => out.extend_from_slice(b"true"),
        Value::Bool(false) => out.extend_from_slice(b"false"),
        Value::Number(n) => out.extend_from_slice(js_number(n).as_bytes()),
        Value::String(s) => write_str(out, s),
        Value::Array(items) => {
            out.push(b'[');
            for (i, item) in items.iter().enumerate() {
                if i > 0 {
                    out.push(b',');
                }
                write(out, item, omit_cache_control);
            }
            out.push(b']');
        }
        Value::Object(map) => write_object(out, map, omit_cache_control),
    }
}

/// The canonical text of `value` on its own.
pub(super) fn text(value: &Value, omit_cache_control: bool) -> Vec<u8> {
    let mut out = Vec::new();
    write(&mut out, value, omit_cache_control);
    out
}

fn write_object(out: &mut Vec<u8>, map: &Map<String, Value>, omit_cache_control: bool) {
    let mut keys: Vec<&String> = map
        .keys()
        .filter(|k| !(omit_cache_control && *k == "cache_control"))
        .collect();
    keys.sort_by(|a, b| utf16_order(a, b));
    out.push(b'{');
    for (i, key) in keys.into_iter().enumerate() {
        if i > 0 {
            out.push(b',');
        }
        write_str(out, key);
        out.push(b':');
        write(out, &map[key], omit_cache_control);
    }
    out.push(b'}');
}

/// JavaScript's default sort: by UTF-16 code units, which differs from code point order for a
/// character above U+FFFF against one in U+E000 to U+FFFF.
pub(super) fn utf16_order(a: &str, b: &str) -> std::cmp::Ordering {
    a.encode_utf16().cmp(b.encode_utf16())
}

/// `JSON.stringify` of a string: only `"`, `\` and control characters are escaped. `/`,
/// non-ASCII characters and U+2028 stay as they are. A Rust string holds no lone surrogate.
pub(super) fn write_str(out: &mut Vec<u8>, s: &str) {
    out.push(b'"');
    let bytes = s.as_bytes();
    let mut start = 0;
    for (i, &b) in bytes.iter().enumerate() {
        let escape: Option<&[u8]> = match b {
            b'"' => Some(b"\\\""),
            b'\\' => Some(b"\\\\"),
            b'\n' => Some(b"\\n"),
            b'\r' => Some(b"\\r"),
            b'\t' => Some(b"\\t"),
            0x08 => Some(b"\\b"),
            0x0c => Some(b"\\f"),
            0x00..=0x1f => None,
            _ => continue,
        };
        out.extend_from_slice(&bytes[start..i]);
        match escape {
            Some(e) => out.extend_from_slice(e),
            None => out.extend_from_slice(format!("\\u{b:04x}").as_bytes()),
        }
        start = i + 1;
    }
    out.extend_from_slice(&bytes[start..]);
    out.push(b'"');
}

/// Above this an integer is a JavaScript double, printed as one.
const MAX_SAFE: u64 = 1 << 53;

/// `JSON.stringify` of a number. The record was written by JavaScript, so every number in it
/// prints back to the text it was read from.
pub(super) fn js_number(n: &Number) -> String {
    if let Some(u) = n.as_u64() {
        return if u < MAX_SAFE {
            u.to_string()
        } else {
            js_double(u as f64)
        };
    }
    if let Some(i) = n.as_i64() {
        return if i.unsigned_abs() < MAX_SAFE {
            i.to_string()
        } else {
            js_double(i as f64)
        };
    }
    js_double(n.as_f64().unwrap_or(f64::NAN))
}

/// `Number.prototype.toString` for a double (ECMA-262, Number::toString): the shortest digits
/// that round-trip, in plain notation from 1e-6 up to 1e21 and in exponent notation outside.
fn js_double(v: f64) -> String {
    if !v.is_finite() {
        return "null".to_owned();
    }
    if v == 0.0 {
        return "0".to_owned();
    }
    let sign = if v < 0.0 { "-" } else { "" };
    // `{:e}` prints the shortest round-trip digits as `d.ddde<exp>`.
    let sci = format!("{:e}", v.abs());
    let (mantissa, exp) = sci.split_once('e').expect("exponent notation");
    let digits: String = mantissa.chars().filter(|c| *c != '.').collect();
    let exp: i64 = exp.parse().expect("an integer exponent");
    let k = digits.len() as i64;
    let n = exp + 1;
    let body = if k <= n && n <= 21 {
        format!("{digits}{}", "0".repeat((n - k) as usize))
    } else if 0 < n && n <= 21 {
        format!("{}.{}", &digits[..n as usize], &digits[n as usize..])
    } else if -6 < n && n <= 0 {
        format!("0.{}{digits}", "0".repeat((-n) as usize))
    } else {
        let e = n - 1;
        let mantissa = if k == 1 {
            digits.clone()
        } else {
            format!("{}.{}", &digits[..1], &digits[1..])
        };
        format!("{mantissa}e{}{}", if e >= 0 { "+" } else { "-" }, e.abs())
    };
    format!("{sign}{body}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn canon(v: &Value) -> String {
        String::from_utf8(text(v, false)).unwrap()
    }

    /// Each expected text is what `JSON.stringify` prints in Node 24 for the same value.
    #[test]
    fn numbers_print_as_javascript_prints_them() {
        let cases = [
            ("0", "0"),
            ("-0", "0"),
            ("1.0", "1"),
            ("100", "100"),
            ("1.5", "1.5"),
            ("0.1", "0.1"),
            ("0.000001", "0.000001"),
            ("1e-7", "1e-7"),
            ("1.5e-7", "1.5e-7"),
            ("123456789012345680000", "123456789012345680000"),
            ("1e21", "1e+21"),
            ("1.2345e25", "1.2345e+25"),
            ("-2.5", "-2.5"),
            ("9007199254740993", "9007199254740992"),
            ("12345678901234567890", "12345678901234567000"),
            ("0.30000000000000004", "0.30000000000000004"),
        ];
        for (input, want) in cases {
            let v: Value = serde_json::from_str(input).unwrap();
            assert_eq!(canon(&v), want, "{input}");
        }
    }

    #[test]
    fn strings_escape_only_quotes_backslashes_and_control_characters() {
        let v = json!("a\"b\\c/d\u{2028}é\u{7f}\u{1f}\u{8}\u{c}\n\r\t\u{0}");
        assert_eq!(
            canon(&v),
            "\"a\\\"b\\\\c/d\u{2028}é\u{7f}\\u001f\\b\\f\\n\\r\\t\\u0000\""
        );
    }

    #[test]
    fn keys_sort_by_utf16_code_units_and_cache_control_can_be_left_out() {
        // U+FF5E sorts after U+1F600 in UTF-16 (0xFF5E > 0xD83D) but before it by code point.
        let v = json!({"b": 1, "\u{1F600}": 2, "\u{FF5E}": 3, "a": {"z": 1, "cache_control": {"type": "ephemeral"}}});
        assert_eq!(
            canon(&v),
            "{\"a\":{\"cache_control\":{\"type\":\"ephemeral\"},\"z\":1},\"b\":1,\"\u{1F600}\":2,\"\u{FF5E}\":3}"
        );
        assert_eq!(
            String::from_utf8(text(&v, true)).unwrap(),
            "{\"a\":{\"z\":1},\"b\":1,\"\u{1F600}\":2,\"\u{FF5E}\":3}"
        );
    }

    #[test]
    fn a_hash_parses_from_hex_and_nothing_else_does() {
        let h = sha256(b"abc");
        let hex: String = h.iter().map(|b| format!("{b:02x}")).collect();
        assert_eq!(
            hex,
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
        assert_eq!(parse_hash(&hex), Some(h));
        assert_eq!(parse_hash("abc"), None);
        assert_eq!(parse_hash(&"g".repeat(64)), None);
    }
}
