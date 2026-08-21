//! SingleStepTests `spc700` vectors (ticket W6-04a; FR-CORE-031).
//!
//! 256 opcode files, 1000 cases each. Same shape as the 65816 suite in
//! `rf-snes/src/cpu/tests/vectors.rs`, and the same two deliberate
//! choices: registers and memory are compared, the cycle trace is not
//! (that needs a cycle-accurate executor); and coverage is DISCOVERED
//! from the run rather than assumed, so an unimplemented opcode reports
//! itself instead of hiding.
//!
//! The bus here is flat, with no I/O decoding. Several vectors poke
//! `$00F0-$00FF` as ordinary memory, so routing those to the timers —
//! which is correct for the real APU — would fail cases that are testing
//! the instruction set rather than the hardware.

use rf_snes::apu::spc700::{ApuBus, FlatApuBus, Spc700};
use sha2 as _;

fn vectors_dir() -> Option<std::path::PathBuf> {
    if let Ok(p) = std::env::var("RF_SPC700_VECTORS") {
        return Some(p.into());
    }
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let base = root.join("roms/snes/singlestep-spc700");
    let mut found = None;
    for e in std::fs::read_dir(&base).ok()? {
        let p = e.ok()?.path().join("v1");
        if p.is_dir() {
            found = Some(p);
        }
    }
    found
}

#[derive(Default, Clone)]
struct State {
    pc: u16,
    a: u8,
    x: u8,
    y: u8,
    sp: u8,
    psw: u8,
    ram: Vec<(u16, u8)>,
}

struct Cursor<'a> {
    b: &'a [u8],
    i: usize,
}

impl<'a> Cursor<'a> {
    fn skip_ws(&mut self) {
        while self.i < self.b.len() && self.b[self.i].is_ascii_whitespace() {
            self.i += 1;
        }
    }
    fn eat(&mut self, c: u8) -> bool {
        self.skip_ws();
        if self.i < self.b.len() && self.b[self.i] == c {
            self.i += 1;
            true
        } else {
            false
        }
    }
    fn expect(&mut self, c: u8) {
        assert!(self.eat(c), "expected {:?} at {}", c as char, self.i);
    }
    fn key(&mut self) -> &'a str {
        self.skip_ws();
        self.expect(b'"');
        let s = self.i;
        while self.b[self.i] != b'"' {
            self.i += 1;
        }
        let k = std::str::from_utf8(&self.b[s..self.i]).expect("ascii");
        self.i += 1;
        self.expect(b':');
        k
    }
    fn number(&mut self) -> i64 {
        self.skip_ws();
        let s = self.i;
        if self.b[self.i] == b'-' {
            self.i += 1;
        }
        while self.i < self.b.len() && self.b[self.i].is_ascii_digit() {
            self.i += 1;
        }
        std::str::from_utf8(&self.b[s..self.i])
            .expect("ascii")
            .parse()
            .expect("int")
    }
    fn skip_value(&mut self) {
        self.skip_ws();
        match self.b[self.i] {
            b'"' => {
                self.i += 1;
                while self.b[self.i] != b'"' {
                    self.i += 1;
                }
                self.i += 1;
            }
            b'n' => self.i += 4, // null
            b'[' | b'{' => {
                let (open, close) = if self.b[self.i] == b'[' {
                    (b'[', b']')
                } else {
                    (b'{', b'}')
                };
                let mut d = 0;
                loop {
                    let c = self.b[self.i];
                    if c == b'"' {
                        self.i += 1;
                        while self.b[self.i] != b'"' {
                            self.i += 1;
                        }
                    } else if c == open {
                        d += 1;
                    } else if c == close {
                        d -= 1;
                        if d == 0 {
                            self.i += 1;
                            return;
                        }
                    }
                    self.i += 1;
                }
            }
            _ => {
                self.number();
            }
        }
    }
}

fn parse_state(cur: &mut Cursor<'_>) -> State {
    let mut st = State::default();
    cur.expect(b'{');
    loop {
        let k = cur.key();
        if k == "ram" {
            cur.expect(b'[');
            if !cur.eat(b']') {
                loop {
                    cur.expect(b'[');
                    let a = cur.number();
                    cur.expect(b',');
                    let v = cur.number();
                    cur.expect(b']');
                    st.ram.push((a as u16, v as u8));
                    if !cur.eat(b',') {
                        break;
                    }
                }
                cur.expect(b']');
            }
        } else {
            let v = cur.number();
            match k {
                "pc" => st.pc = v as u16,
                "a" => st.a = v as u8,
                "x" => st.x = v as u8,
                "y" => st.y = v as u8,
                "sp" => st.sp = v as u8,
                "psw" => st.psw = v as u8,
                other => panic!("unknown key {other}"),
            }
        }
        if !cur.eat(b',') {
            break;
        }
    }
    cur.expect(b'}');
    st
}

struct Vector {
    name: String,
    initial: State,
    final_: State,
}

fn parse_file(b: &[u8]) -> Vec<Vector> {
    let mut cur = Cursor { b, i: 0 };
    let mut out = Vec::new();
    cur.expect(b'[');
    if cur.eat(b']') {
        return out;
    }
    loop {
        cur.expect(b'{');
        let (mut name, mut initial, mut final_) =
            (String::new(), State::default(), State::default());
        loop {
            let k = cur.key();
            match k {
                "name" => {
                    cur.skip_ws();
                    cur.expect(b'"');
                    let s = cur.i;
                    while cur.b[cur.i] != b'"' {
                        cur.i += 1;
                    }
                    name = String::from_utf8_lossy(&cur.b[s..cur.i]).into_owned();
                    cur.i += 1;
                }
                "initial" => initial = parse_state(&mut cur),
                "final" => final_ = parse_state(&mut cur),
                _ => cur.skip_value(),
            }
            if !cur.eat(b',') {
                break;
            }
        }
        cur.expect(b'}');
        out.push(Vector {
            name,
            initial,
            final_,
        });
        if !cur.eat(b',') {
            break;
        }
    }
    out
}

fn run_one(v: &Vector) -> Result<(), String> {
    // Built through `new()` rather than a struct literal: ticket W7-08
    // added a crate-private `branch_taken` field, which an integration
    // test (a separate crate) cannot name. Assigning the architectural
    // registers is also the honest shape — those are the vector's inputs;
    // `branch_taken` is a per-step output, not initial state.
    let mut cpu = Spc700::new();
    cpu.a = v.initial.a;
    cpu.x = v.initial.x;
    cpu.y = v.initial.y;
    cpu.sp = v.initial.sp;
    cpu.pc = v.initial.pc;
    cpu.psw = v.initial.psw;
    cpu.stopped = false;
    let mut bus = FlatApuBus::new();
    for (a, val) in &v.initial.ram {
        bus.mem[*a as usize] = *val;
    }
    cpu.step(&mut bus)
        .map_err(|op| format!("unimplemented opcode {op:#04X}"))?;

    let mut wrong = Vec::new();
    let mut chk = |n: &str, got: u32, want: u32| {
        if got != want {
            wrong.push(format!("{n}: got {got:#06X} want {want:#06X}"));
        }
    };
    chk("pc", cpu.pc.into(), v.final_.pc.into());
    chk("a", cpu.a.into(), v.final_.a.into());
    chk("x", cpu.x.into(), v.final_.x.into());
    chk("y", cpu.y.into(), v.final_.y.into());
    chk("sp", cpu.sp.into(), v.final_.sp.into());
    chk("psw", cpu.psw.into(), v.final_.psw.into());
    for (a, want) in &v.final_.ram {
        let got = bus.peek(*a);
        if got != *want {
            wrong.push(format!("ram[{a:#06X}]: got {got:#04X} want {want:#04X}"));
        }
    }
    if wrong.is_empty() {
        Ok(())
    } else {
        Err(wrong.join("; "))
    }
}

#[test]
#[ignore = "local vector suite"]
fn singlestep_spc700_vectors() {
    let Some(dir) = vectors_dir() else {
        eprintln!("SKIP: extract roms/snes/singlestep-spc700-67d15f4.zip");
        return;
    };
    let mut files: Vec<_> = std::fs::read_dir(&dir)
        .expect("readable")
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|x| x == "json"))
        .collect();
    files.sort();
    assert!(
        !files.is_empty(),
        "a suite that ran zero cases is not a pass"
    );

    let (mut pass, mut fail) = (0usize, 0usize);
    let mut unimplemented = std::collections::BTreeSet::new();
    let mut report = Vec::new();
    for path in &files {
        let bytes = std::fs::read(path).expect("readable");
        let stem = path
            .file_stem()
            .unwrap_or_default()
            .to_string_lossy()
            .into_owned();
        let (mut p, mut f) = (0usize, 0usize);
        let mut first = Vec::new();
        let mut unimpl = 0usize;
        let vectors = parse_file(&bytes);
        for v in &vectors {
            match run_one(v) {
                Ok(()) => p += 1,
                Err(why) => {
                    f += 1;
                    if why.starts_with("unimplemented") {
                        unimpl += 1;
                    }
                    if first.len() < 2 {
                        first.push(format!("{}: {why}", v.name));
                    }
                }
            }
        }
        if unimpl == vectors.len() && !vectors.is_empty() {
            unimplemented.insert(stem.clone());
            continue;
        }
        pass += p;
        fail += f;
        if f > 0 {
            report.push(format!(
                "{stem}: {f}/{} failed\n    {}",
                p + f,
                first.join("\n    ")
            ));
        }
    }
    eprintln!(
        "spc700 vectors: {pass} passed, {fail} failed ({} cases)",
        pass + fail
    );
    eprintln!(
        "  coverage: {} of 256 opcodes tested; {} unimplemented: {:?}",
        256 - unimplemented.len(),
        unimplemented.len(),
        unimplemented
    );
    for r in report.iter().take(12) {
        eprintln!("{r}");
    }
    assert!(
        fail == 0 && unimplemented.is_empty(),
        "spc700 vectors failed"
    );
}

/// **The cycle table and its oracle must not disagree** (ticket W7-08).
///
/// `spc700::timing::CYCLES` was DERIVED from these vectors rather than
/// transcribed from a document, and this test re-derives it from the same
/// data. Writing 256 numbers by hand is how a plausible, subtly wrong
/// emulator gets made; deriving them is only safe if the derivation is
/// checked, which is what this is.
///
/// It also pins the two facts the table's shape depends on: exactly the
/// branching opcodes vary, and a taken branch always costs `+2`.
#[test]
#[ignore = "needs the fetched SPC700 vectors (NFR-006); run via scripts/local-gate.sh"]
fn spc700_cycle_table_matches_the_vectors() {
    use rf_snes::apu::spc700::timing;

    let Some(dir) = vectors_dir() else {
        eprintln!("SKIP: run scripts/fetch-test-roms.sh singlestep-spc700");
        return;
    };

    let mut checked = 0usize;
    let mut branching_seen = Vec::new();
    for opcode in 0u16..=255 {
        let path = dir.join(format!("{opcode:02x}.json"));
        let Ok(text) = std::fs::read_to_string(&path) else {
            continue;
        };
        // Every `"cycles": [...]` array's length is one execution's cycle
        // count; collect the distinct lengths this opcode produces.
        let mut lengths: Vec<usize> = Vec::new();
        for case in text.split("\"cycles\":").skip(1) {
            let Some(open) = case.find('[') else { continue };
            let mut depth = 0i32;
            let mut count = 0usize;
            for (i, ch) in case[open..].char_indices() {
                match ch {
                    '[' => {
                        depth += 1;
                        if depth == 2 {
                            count += 1;
                        }
                    }
                    ']' => {
                        depth -= 1;
                        if depth == 0 {
                            break;
                        }
                    }
                    _ => {}
                }
                let _ = i;
            }
            if !lengths.contains(&count) {
                lengths.push(count);
            }
        }
        if lengths.is_empty() {
            continue;
        }
        lengths.sort_unstable();
        let op = u8::try_from(opcode).expect("0..=255");
        let base = u8::try_from(lengths[0]).expect("cycle counts fit a byte");
        assert_eq!(
            timing::CYCLES[opcode as usize],
            base,
            "opcode ${op:02X}: table says {} cycles, the vectors say {base}",
            timing::CYCLES[opcode as usize]
        );
        if lengths.len() > 1 {
            branching_seen.push(op);
            assert_eq!(
                lengths.len(),
                2,
                "opcode ${op:02X} shows {} distinct cycle counts; the table models only \
                 not-taken and taken",
                lengths.len()
            );
            let extra = u8::try_from(lengths[1] - lengths[0]).expect("fits");
            assert_eq!(
                extra,
                timing::BRANCH_TAKEN_EXTRA,
                "opcode ${op:02X} costs +{extra} when taken, not +{}",
                timing::BRANCH_TAKEN_EXTRA
            );
            assert!(
                timing::is_branching(op),
                "opcode ${op:02X} varies with the branch but is not in BRANCHING"
            );
        } else {
            assert!(
                !timing::is_branching(op),
                "opcode ${op:02X} is listed as branching but its cost never varies"
            );
        }
        checked += 1;
    }

    assert_eq!(checked, 256, "every opcode must be checked, saw {checked}");
    branching_seen.sort_unstable();
    let mut declared = timing::BRANCHING.to_vec();
    declared.sort_unstable();
    assert_eq!(
        branching_seen, declared,
        "the BRANCHING list and the vectors disagree about which opcodes vary"
    );
}
