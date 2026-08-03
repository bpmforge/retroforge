//! Minimal hand-rolled JSON reader for the SingleStepTests `nes6502`
//! vector files.
//!
//! `crates/rf-nes/Cargo.toml` is only in this ticket's write scope for the
//! criterion dev-dependency (see plan.json W1-01a), so a real JSON crate
//! (`serde_json`) is off the table here — this parses exactly the schema
//! observed in the real vector files (verified directly against
//! `roms/nes/singlestep-nes6502-src/nes6502/v1/a9.json` et al. — see
//! `super::vectors` module doc) and nothing more general. It borrows `&str`
//! slices from the input buffer rather than allocating, since these files
//! run into the megabytes.
use std::str;

/// One `initial`/`final` CPU-state block: `{"pc":.., "s":.., "a":.., "x":..,
/// "y":.., "p":.., "ram":[[addr,value],...]}`.
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

    /// If the next non-whitespace byte is `b`, consume it and return true.
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

    /// Parses a JSON string with no escape handling: sufficient for this
    /// dataset, whose only strings are hex-opcode test names (e.g. `"a9 c3
    /// 7a"`) and the `"read"`/`"write"` cycle tags — none contain `"` or
    /// `\`.
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

    /// Skips one JSON value of any shape — used for object keys this
    /// reader doesn't care about, so an upstream schema addition doesn't
    /// break parsing.
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
