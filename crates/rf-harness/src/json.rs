//! Minimal, dependency-free JSON writer for the accuracy-table output
//! (ticket W0-03). Write-only by design — this crate never needs to parse
//! JSON back in, only emit it, so a hand-rolled writer avoids adding
//! `serde_json` (a new dependency + TECH_STACK row) for one small, fixed
//! output shape.
//!
//! The escaper is the load-bearing part: `$6004+` message text is
//! arbitrary bytes authored by a third-party test ROM and can legally
//! contain `"`, `\`, newlines, and other control characters — see
//! `escapes_hostile_characters` below for the test that exercises this.

use std::fmt::Write as _;

/// A JSON value restricted to what the local-gate evidence file needs: no
/// floats — every field is a string, integer, bool, null, or nested
/// array/object. `Null` was added by ticket W1-03 for
/// `local_gate_evidence`'s nestest row (`first_divergence: null` when every
/// line matched) — the original W0-03 accuracy-table shape never needed it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Json {
    Str(String),
    Int(i64),
    Bool(bool),
    Null,
    Array(Vec<Json>),
    Object(Vec<(String, Json)>),
}

impl Json {
    #[must_use]
    pub fn str(s: impl Into<String>) -> Json {
        Json::Str(s.into())
    }

    #[must_use]
    pub fn object(fields: Vec<(&str, Json)>) -> Json {
        Json::Object(
            fields
                .into_iter()
                .map(|(k, v)| (k.to_string(), v))
                .collect(),
        )
    }

    /// Render as compact JSON text.
    #[must_use]
    pub fn to_json_string(&self) -> String {
        let mut out = String::new();
        self.write(&mut out);
        out
    }

    fn write(&self, out: &mut String) {
        match self {
            Json::Str(s) => write_json_string(s, out),
            Json::Int(n) => {
                let _ = write!(out, "{n}");
            }
            Json::Bool(b) => {
                out.push_str(if *b { "true" } else { "false" });
            }
            Json::Null => out.push_str("null"),
            Json::Array(items) => {
                out.push('[');
                for (i, item) in items.iter().enumerate() {
                    if i > 0 {
                        out.push(',');
                    }
                    item.write(out);
                }
                out.push(']');
            }
            Json::Object(fields) => {
                out.push('{');
                for (i, (k, v)) in fields.iter().enumerate() {
                    if i > 0 {
                        out.push(',');
                    }
                    write_json_string(k, out);
                    out.push(':');
                    v.write(out);
                }
                out.push('}');
            }
        }
    }
}

/// Escape `s` per RFC 8259 §7 and append the quoted result to `out`:
/// `"`, `\`, and every control character (`< 0x20`) are escaped; the
/// named short escapes (`\n`, `\t`, `\r`, `\"`, `\\`) are used where they
/// exist, everything else `< 0x20` becomes `\u00XX`.
fn write_json_string(s: &str, out: &mut String) {
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => {
                let _ = write!(out, "\\u{:04x}", c as u32);
            }
            c => out.push(c),
        }
    }
    out.push('"');
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn renders_object_and_array() {
        let v = Json::object(vec![
            ("suite", Json::str("instr_test-v5")),
            ("pass", Json::Bool(true)),
            ("frame", Json::Int(812)),
            ("rows", Json::Array(vec![Json::str("a"), Json::str("b")])),
        ]);
        assert_eq!(
            v.to_json_string(),
            r#"{"suite":"instr_test-v5","pass":true,"frame":812,"rows":["a","b"]}"#
        );
    }

    /// The load-bearing test (advisor guidance): a `$6004+`-style message
    /// carrying quotes, backslashes, newlines, and a raw control byte must
    /// round-trip through *some* JSON parser without corrupting the
    /// document structure. We don't depend on one here, but we can assert
    /// the exact escaped form RFC 8259 requires.
    #[test]
    fn escapes_hostile_characters() {
        let hostile = "line1\nline2\t\"quoted\"\\backslash\u{0001}";
        let escaped = Json::str(hostile).to_json_string();
        assert_eq!(
            escaped,
            "\"line1\\nline2\\t\\\"quoted\\\"\\\\backslash\\u0001\""
        );
        // And the structural invariant that actually matters: no raw
        // unescaped quote or backslash appears inside the payload region.
        let inner = &escaped[1..escaped.len() - 1];
        assert!(!inner.contains("\\\"\"")); // no unescaped quote artifact
    }

    #[test]
    fn empty_string_and_object_render_correctly() {
        assert_eq!(Json::str("").to_json_string(), "\"\"");
        assert_eq!(Json::object(vec![]).to_json_string(), "{}");
        assert_eq!(Json::Array(vec![]).to_json_string(), "[]");
    }

    #[test]
    fn null_renders_as_the_bare_json_literal() {
        assert_eq!(Json::Null.to_json_string(), "null");
        let v = Json::object(vec![("first_divergence", Json::Null)]);
        assert_eq!(v.to_json_string(), r#"{"first_divergence":null}"#);
    }
}
