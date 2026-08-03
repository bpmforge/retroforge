//! Minimal hand-rolled JSON reader for the SingleStepTests `nes6502`
//! vector files (ticket W0-07 — `nes6502_evidence` needs to read the same
//! vector files `crates/rf-nes/src/cpu/tests/json.rs` does, but that
//! module is `#[cfg(test)]`-gated inside `rf-nes` and therefore does not
//! exist in a normal `cargo build`; `crates/rf-nes/**` is outside this
//! ticket's write scope, so this is a second, independent reader for the
//! identical schema rather than a shared dependency).
//!
//! Deliberately not a general JSON parser: this reads exactly the shape
//! observed in the real vector files (`{"name":.., "initial":{"pc":..,
//! "s":.., "a":.., "x":.., "y":.., "p":.., "ram":[[addr,value],...]},
//! "final":{...}, "cycles":[[addr,value,"read"|"write"],...]}`), nothing
//! more general — adding a real JSON crate here would be a new
//! TECH_STACK.md dependency for a fixed, already-known input shape rf-nes
//! itself didn't need one for either.
use std::str;

/// One `initial`/`final` CPU-state block.
#[derive(Default, Clone)]
pub(crate) struct CpuState {
    pub pc: u16,
    pub s: u8,
    pub a: u8,
    pub x: u8,
    pub y: u8,
    pub p: u8,
    pub ram: Vec<(u16, u8)>,
}

/// One entry of the `cycles` array: `[address, value, "read"|"write"]`.
pub(crate) struct CycleOp {
    pub addr: u16,
    pub value: u8,
    pub is_write: bool,
}

/// One test case: `{"name":.., "initial":.., "final":.., "cycles":..}`.
pub(crate) struct Vector<'a> {
    pub name: &'a str,
    pub initial: CpuState,
    pub expected_final: CpuState,
    pub cycles: Vec<CycleOp>,
}

struct Cursor<'a> {
    bytes: &'a [u8],
    pos: usize,
}

impl<'a> Cursor<'a> {
    fn new(bytes: &'a [u8]) -> Self {
        Cursor { bytes, pos: 0 }
    }

    fn skip_ws(&mut self) {
        while self.pos < self.bytes.len() && self.bytes[self.pos].is_ascii_whitespace() {
            self.pos += 1;
        }
    }

    fn cur(&self) -> u8 {
        self.bytes[self.pos]
    }

    fn expect(&mut self, b: u8) {
        self.skip_ws();
        assert_eq!(
            self.cur(),
            b,
            "vector JSON: expected {:?} at byte {}, found {:?}",
            b as char,
            self.pos,
            self.cur() as char
        );
        self.pos += 1;
    }

    fn try_consume(&mut self, b: u8) -> bool {
        self.skip_ws();
        if self.pos < self.bytes.len() && self.cur() == b {
            self.pos += 1;
            true
        } else {
            false
        }
    }

    fn parse_i64(&mut self) -> i64 {
        self.skip_ws();
        let neg = if self.cur() == b'-' {
            self.pos += 1;
            true
        } else {
            false
        };
        let mut value: i64 = 0;
        while self.pos < self.bytes.len() && self.cur().is_ascii_digit() {
            value = value * 10 + i64::from(self.cur() - b'0');
            self.pos += 1;
        }
        if neg {
            -value
        } else {
            value
        }
    }

    /// No escape handling: this dataset's only strings are hex-opcode test
    /// names and the `"read"`/`"write"` cycle tags, none of which contain
    /// `"` or `\`.
    fn parse_str(&mut self) -> &'a str {
        self.expect(b'"');
        let start = self.pos;
        while self.cur() != b'"' {
            self.pos += 1;
        }
        let s = str::from_utf8(&self.bytes[start..self.pos]).expect("vector JSON: non-utf8 string");
        self.pos += 1;
        s
    }

    fn parse_rw_is_write(&mut self) -> bool {
        self.expect(b'"');
        let is_write = self.cur() == b'w';
        while self.cur() != b'"' {
            self.pos += 1;
        }
        self.pos += 1;
        is_write
    }

    fn skip_value(&mut self) {
        self.skip_ws();
        match self.cur() {
            b'"' => {
                self.parse_str();
            }
            b'[' => {
                self.pos += 1;
                if !self.try_consume(b']') {
                    loop {
                        self.skip_value();
                        if self.try_consume(b',') {
                            continue;
                        }
                        self.expect(b']');
                        break;
                    }
                }
            }
            b'{' => {
                self.pos += 1;
                if !self.try_consume(b'}') {
                    loop {
                        self.parse_str();
                        self.expect(b':');
                        self.skip_value();
                        if self.try_consume(b',') {
                            continue;
                        }
                        self.expect(b'}');
                        break;
                    }
                }
            }
            b't' => self.pos += 4,
            b'f' => self.pos += 5,
            b'n' => self.pos += 4,
            _ => {
                if self.cur() == b'-' {
                    self.pos += 1;
                }
                while self.pos < self.bytes.len()
                    && (self.cur().is_ascii_digit() || self.cur() == b'.')
                {
                    self.pos += 1;
                }
            }
        }
    }
}

fn parse_ram(cur: &mut Cursor<'_>) -> Vec<(u16, u8)> {
    cur.expect(b'[');
    let mut out = Vec::new();
    if cur.try_consume(b']') {
        return out;
    }
    loop {
        cur.expect(b'[');
        let addr = cur.parse_i64() as u16;
        cur.expect(b',');
        let val = cur.parse_i64() as u8;
        cur.expect(b']');
        out.push((addr, val));
        if cur.try_consume(b',') {
            continue;
        }
        cur.expect(b']');
        break;
    }
    out
}

fn parse_cpu_state(cur: &mut Cursor<'_>) -> CpuState {
    cur.expect(b'{');
    let mut state = CpuState::default();
    if cur.try_consume(b'}') {
        return state;
    }
    loop {
        let key = cur.parse_str();
        cur.expect(b':');
        match key {
            "pc" => state.pc = cur.parse_i64() as u16,
            "s" => state.s = cur.parse_i64() as u8,
            "a" => state.a = cur.parse_i64() as u8,
            "x" => state.x = cur.parse_i64() as u8,
            "y" => state.y = cur.parse_i64() as u8,
            "p" => state.p = cur.parse_i64() as u8,
            "ram" => state.ram = parse_ram(cur),
            _ => cur.skip_value(),
        }
        if cur.try_consume(b',') {
            continue;
        }
        cur.expect(b'}');
        break;
    }
    state
}

fn parse_cycles(cur: &mut Cursor<'_>) -> Vec<CycleOp> {
    cur.expect(b'[');
    let mut out = Vec::new();
    if cur.try_consume(b']') {
        return out;
    }
    loop {
        cur.expect(b'[');
        let addr = cur.parse_i64() as u16;
        cur.expect(b',');
        let value = cur.parse_i64() as u8;
        cur.expect(b',');
        let is_write = cur.parse_rw_is_write();
        cur.expect(b']');
        out.push(CycleOp {
            addr,
            value,
            is_write,
        });
        if cur.try_consume(b',') {
            continue;
        }
        cur.expect(b']');
        break;
    }
    out
}

fn parse_vector<'a>(cur: &mut Cursor<'a>) -> Vector<'a> {
    cur.expect(b'{');
    let mut name = "";
    let mut initial = CpuState::default();
    let mut expected_final = CpuState::default();
    let mut cycles = Vec::new();
    if !cur.try_consume(b'}') {
        loop {
            let key = cur.parse_str();
            cur.expect(b':');
            match key {
                "name" => name = cur.parse_str(),
                "initial" => initial = parse_cpu_state(cur),
                "final" => expected_final = parse_cpu_state(cur),
                "cycles" => cycles = parse_cycles(cur),
                _ => cur.skip_value(),
            }
            if cur.try_consume(b',') {
                continue;
            }
            cur.expect(b'}');
            break;
        }
    }
    Vector {
        name,
        initial,
        expected_final,
        cycles,
    }
}

/// Parses a whole `nes6502/v1/<opcode>.json` file (a top-level array of
/// test cases).
pub(crate) fn parse_vectors(bytes: &[u8]) -> Vec<Vector<'_>> {
    let mut cur = Cursor::new(bytes);
    cur.expect(b'[');
    let mut out = Vec::new();
    if cur.try_consume(b']') {
        return out;
    }
    loop {
        out.push(parse_vector(&mut cur));
        if cur.try_consume(b',') {
            continue;
        }
        cur.expect(b']');
        break;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_a_minimal_single_case_file() {
        let json = br#"[{"name":"a9 00 00","initial":{"pc":100,"s":253,"a":0,"x":0,"y":0,"p":36,"ram":[[100,169],[101,0]]},"final":{"pc":102,"s":253,"a":0,"x":0,"y":0,"p":38,"ram":[[100,169],[101,0]]},"cycles":[[100,169,"read"],[101,0,"read"]]}]"#;
        let vectors = parse_vectors(json);
        assert_eq!(vectors.len(), 1);
        let v = &vectors[0];
        assert_eq!(v.name, "a9 00 00");
        assert_eq!(v.initial.pc, 100);
        assert_eq!(v.initial.ram, vec![(100, 169), (101, 0)]);
        assert_eq!(v.expected_final.p, 38);
        assert_eq!(v.cycles.len(), 2);
        assert_eq!(v.cycles[0].addr, 100);
        assert_eq!(v.cycles[0].value, 169);
        assert!(!v.cycles[0].is_write);
    }

    #[test]
    fn parses_multiple_cases_and_a_write_cycle() {
        let json = br#"[
            {"name":"c1","initial":{"pc":1,"s":2,"a":3,"x":4,"y":5,"p":6,"ram":[]},"final":{"pc":7,"s":8,"a":9,"x":10,"y":11,"p":12,"ram":[[0,1]]},"cycles":[[1,2,"write"]]},
            {"name":"c2","initial":{"pc":0,"s":0,"a":0,"x":0,"y":0,"p":0,"ram":[]},"final":{"pc":0,"s":0,"a":0,"x":0,"y":0,"p":0,"ram":[]},"cycles":[]}
        ]"#;
        let vectors = parse_vectors(json);
        assert_eq!(vectors.len(), 2);
        assert_eq!(vectors[0].name, "c1");
        assert!(vectors[0].cycles[0].is_write);
        assert_eq!(vectors[1].name, "c2");
        assert!(vectors[1].cycles.is_empty());
    }

    #[test]
    fn empty_array_parses_to_no_vectors() {
        assert_eq!(parse_vectors(b"[]").len(), 0);
    }
}
