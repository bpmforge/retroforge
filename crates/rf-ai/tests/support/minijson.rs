//! Minimal, dependency-free JSON reader/writer for the GPU-pass and
//! local-AI benchmark evidence file (ticket W16-01;
//! `docs/design/ENHANCEMENT_WAVE_16.md` §8; `docs/evidence/gpu-passes.json`).
//!
//! Hand-rolled rather than pulling in `serde_json`: neither `rf-renderer`
//! nor `rf-ai` otherwise depends on a JSON crate, and this ticket's whole
//! evidence shape is one flat document (`{machine, generated_at, rows:
//! [ {..flat string/number/bool fields..}, ... ]}`) — a full serde
//! dependency would be one dependency for one file. Supports exactly what
//! this evidence file needs: objects, arrays, strings, numbers, bools,
//! null. Deliberately duplicated verbatim in
//! `crates/rf-ai/tests/onnx_bench.rs` — the two evidence producers (this
//! crate's shader-chain/neural-stub bench, `rf-ai`'s ONNX/MetalFX spikes)
//! sit on opposite sides of a crate boundary this ticket's `write_scope`
//! has no shared crate to bridge.
//!
//! Merge contract: [`Doc::merge_rows`] replaces any existing row whose
//! `pass`+`size` fields match an incoming row, and appends the rest —
//! so re-running one producer (say, the shader-chain bench) updates its
//! own rows in place without clobbering another producer's rows (say, the
//! ONNX spike's) already in the file.

use std::collections::BTreeMap;

#[derive(Debug, Clone, PartialEq)]
pub enum JVal {
    Str(String),
    Num(f64),
    Bool(bool),
    Null,
    Arr(Vec<JVal>),
    Obj(Vec<(String, JVal)>),
}

impl JVal {
    pub fn as_str(&self) -> Option<&str> {
        match self {
            JVal::Str(s) => Some(s),
            _ => None,
        }
    }

    fn write(&self, out: &mut String) {
        match self {
            JVal::Str(s) => {
                out.push('"');
                for c in s.chars() {
                    match c {
                        '"' => out.push_str("\\\""),
                        '\\' => out.push_str("\\\\"),
                        '\n' => out.push_str("\\n"),
                        _ => out.push(c),
                    }
                }
                out.push('"');
            }
            JVal::Num(n) => out.push_str(&format!("{n}")),
            JVal::Bool(b) => out.push_str(if *b { "true" } else { "false" }),
            JVal::Null => out.push_str("null"),
            JVal::Arr(items) => {
                out.push('[');
                for (i, it) in items.iter().enumerate() {
                    if i > 0 {
                        out.push(',');
                    }
                    it.write(out);
                }
                out.push(']');
            }
            JVal::Obj(fields) => {
                out.push('{');
                for (i, (k, v)) in fields.iter().enumerate() {
                    if i > 0 {
                        out.push(',');
                    }
                    JVal::Str(k.clone()).write(out);
                    out.push(':');
                    v.write(out);
                }
                out.push('}');
            }
        }
    }

    pub fn to_json_string(&self) -> String {
        let mut s = String::new();
        self.write(&mut s);
        s
    }
}

/// Row helper: an ordered `field -> value` map, kept as `Vec` (not
/// `BTreeMap`) so key order in the written JSON matches insertion order
/// (nicer to read/diff than alphabetical).
pub type RowFields = Vec<(String, JVal)>;

pub fn row_num(fields: &mut RowFields, key: &str, v: f64) {
    fields.push((key.to_string(), JVal::Num(v)));
}
pub fn row_str(fields: &mut RowFields, key: &str, v: &str) {
    fields.push((key.to_string(), JVal::Str(v.to_string())));
}

/// A small recursive-descent JSON parser. Panics on malformed input — the
/// only caller is this ticket's own evidence file, which nothing but
/// these two producers ever writes, so a parse failure means a hand-edit
/// broke the file and should be loud, not silently ignored.
pub struct Parser<'a> {
    bytes: &'a [u8],
    pos: usize,
}

impl<'a> Parser<'a> {
    fn new(s: &'a str) -> Self {
        Parser {
            bytes: s.as_bytes(),
            pos: 0,
        }
    }

    fn skip_ws(&mut self) {
        while self.pos < self.bytes.len() && self.bytes[self.pos].is_ascii_whitespace() {
            self.pos += 1;
        }
    }

    fn peek(&self) -> Option<u8> {
        self.bytes.get(self.pos).copied()
    }

    fn expect(&mut self, c: u8) {
        self.skip_ws();
        assert_eq!(
            self.peek(),
            Some(c),
            "minijson: expected '{}' at byte {}",
            c as char,
            self.pos
        );
        self.pos += 1;
    }

    pub fn parse_value(&mut self) -> JVal {
        self.skip_ws();
        match self.peek() {
            Some(b'{') => self.parse_obj(),
            Some(b'[') => self.parse_arr(),
            Some(b'"') => JVal::Str(self.parse_string()),
            Some(b't') => {
                self.pos += 4;
                JVal::Bool(true)
            }
            Some(b'f') => {
                self.pos += 5;
                JVal::Bool(false)
            }
            Some(b'n') => {
                self.pos += 4;
                JVal::Null
            }
            Some(_) => self.parse_num(),
            None => panic!("minijson: unexpected end of input"),
        }
    }

    fn parse_obj(&mut self) -> JVal {
        self.expect(b'{');
        let mut fields = Vec::new();
        self.skip_ws();
        if self.peek() == Some(b'}') {
            self.pos += 1;
            return JVal::Obj(fields);
        }
        loop {
            self.skip_ws();
            let key = self.parse_string();
            self.expect(b':');
            let val = self.parse_value();
            fields.push((key, val));
            self.skip_ws();
            match self.peek() {
                Some(b',') => {
                    self.pos += 1;
                }
                Some(b'}') => {
                    self.pos += 1;
                    break;
                }
                other => panic!("minijson: expected ',' or '}}' in object, got {other:?}"),
            }
        }
        JVal::Obj(fields)
    }

    fn parse_arr(&mut self) -> JVal {
        self.expect(b'[');
        let mut items = Vec::new();
        self.skip_ws();
        if self.peek() == Some(b']') {
            self.pos += 1;
            return JVal::Arr(items);
        }
        loop {
            items.push(self.parse_value());
            self.skip_ws();
            match self.peek() {
                Some(b',') => {
                    self.pos += 1;
                }
                Some(b']') => {
                    self.pos += 1;
                    break;
                }
                other => panic!("minijson: expected ',' or ']' in array, got {other:?}"),
            }
        }
        JVal::Arr(items)
    }

    fn parse_string(&mut self) -> String {
        self.expect(b'"');
        let mut s = String::new();
        loop {
            let c = self.peek().expect("minijson: unterminated string");
            self.pos += 1;
            match c {
                b'"' => break,
                b'\\' => {
                    let esc = self.peek().expect("minijson: bad escape");
                    self.pos += 1;
                    match esc {
                        b'"' => s.push('"'),
                        b'\\' => s.push('\\'),
                        b'n' => s.push('\n'),
                        b't' => s.push('\t'),
                        other => s.push(other as char),
                    }
                }
                other => s.push(other as char),
            }
        }
        s
    }

    fn parse_num(&mut self) -> JVal {
        let start = self.pos;
        while let Some(c) = self.peek() {
            if c.is_ascii_digit() || matches!(c, b'-' | b'+' | b'.' | b'e' | b'E') {
                self.pos += 1;
            } else {
                break;
            }
        }
        let text = std::str::from_utf8(&self.bytes[start..self.pos]).unwrap();
        JVal::Num(text.parse().expect("minijson: bad number"))
    }
}

pub fn parse(s: &str) -> JVal {
    Parser::new(s).parse_value()
}

/// Read `path`'s existing rows (empty if the file does not exist or fails
/// to parse — a from-scratch evidence file is a normal first run, not an
/// error).
pub fn read_existing_rows(path: &std::path::Path) -> Vec<RowFields> {
    let Ok(text) = std::fs::read_to_string(path) else {
        return Vec::new();
    };
    let JVal::Obj(top) = parse(&text) else {
        return Vec::new();
    };
    let Some((_, JVal::Arr(rows))) = top.into_iter().find(|(k, _)| k == "rows") else {
        return Vec::new();
    };
    rows.into_iter()
        .filter_map(|r| match r {
            JVal::Obj(fields) => Some(fields),
            _ => None,
        })
        .collect()
}

fn field<'a>(row: &'a RowFields, key: &str) -> Option<&'a JVal> {
    row.iter().find(|(k, _)| k == key).map(|(_, v)| v)
}

/// Merge `new_rows` into `existing`: a new row replaces any existing row
/// with the same `(pass, size)` key, and is appended otherwise. Order is
/// preserved (existing rows keep their position; genuinely new ones go to
/// the end) so re-running a producer doesn't reshuffle the file.
#[must_use]
pub fn merge_rows(existing: Vec<RowFields>, new_rows: Vec<RowFields>) -> Vec<RowFields> {
    let key_of = |r: &RowFields| -> (String, String) {
        (
            field(r, "pass")
                .and_then(JVal::as_str)
                .unwrap_or("")
                .to_string(),
            field(r, "size")
                .and_then(JVal::as_str)
                .unwrap_or("")
                .to_string(),
        )
    };
    let mut by_key: BTreeMap<(String, String), RowFields> =
        existing.into_iter().map(|r| (key_of(&r), r)).collect();
    let mut order: Vec<(String, String)> = by_key.keys().cloned().collect();
    for r in new_rows {
        let k = key_of(&r);
        if !by_key.contains_key(&k) {
            order.push(k.clone());
        }
        by_key.insert(k, r);
    }
    order
        .into_iter()
        .filter_map(|k| by_key.remove(&k))
        .collect()
}

/// Write the full evidence document: `{machine, generated_at, rows: [...]}`.
pub fn write_doc(path: &std::path::Path, machine: &str, generated_at: &str, rows: Vec<RowFields>) {
    let doc = JVal::Obj(vec![
        ("machine".to_string(), JVal::Str(machine.to_string())),
        (
            "generated_at".to_string(),
            JVal::Str(generated_at.to_string()),
        ),
        (
            "rows".to_string(),
            JVal::Arr(rows.into_iter().map(JVal::Obj).collect()),
        ),
    ]);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).expect("minijson: could not create evidence directory");
    }
    std::fs::write(path, doc.to_json_string()).expect("minijson: could not write evidence file");
}
