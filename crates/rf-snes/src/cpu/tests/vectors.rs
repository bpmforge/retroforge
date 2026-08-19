//! SingleStepTests `65816` vectors (ticket W6-01a; FR-CORE-030).
//!
//! ## The archive question, settled
//!
//! `tests/rom-manifest.toml` deferred a decision to "the ticket that
//! actually consumes these vectors": fetch a 490 MB archive per run, or
//! curate a leaner per-opcode subset. Neither, as it turns out — the
//! upstream repository is **already per-opcode**. `v1/` holds 512 files,
//! `<opcode>.e.json` and `<opcode>.n.json`, one pair per opcode for
//! emulation and native mode, ~4.5 MB each and ~2.9 GB in total.
//!
//! So the fetcher takes a LIST of opcodes and pulls only those files.
//! There is no archive to download and no subset to hand-curate: the
//! granularity the manifest wanted already exists upstream, and the gate
//! grows one file at a time.
//!
//! That the files come in `.e`/`.n` pairs is a gift for this ticket
//! specifically — the acceptance says "incl. m/x width handling", and
//! emulation mode is precisely where those flags are forced. Both halves
//! of every opcode are run, and a run that found only one half asserts
//! rather than quietly covering half the behaviour.
//!
//! ## What is compared, and what is not
//!
//! **Registers and memory, not cycles.** Each vector carries a
//! cycle-by-cycle bus trace, and this runner deliberately ignores it:
//! W6-01a builds the CPU's *behaviour*, and the master-cycle memory-speed
//! model is W6-01b. Comparing traces now would fail every vector for a
//! reason this ticket is not about, and weakening the comparison later to
//! make them pass would be worse. When W6-01b lands, the trace is right
//! here waiting.
//!
//! Stated rather than left implicit, because "vectors pass" reads as
//! stronger than it is if nobody says which half passed.
//!
//! ## Running them
//!
//! Local only — CI has no ROMs and never fetches vectors (NFR-006), the
//! same posture the nes6502 suite has. `scripts/fetch-65816-vectors.sh`
//! pulls the files; the test skips with a message when they are absent.

use std::path::PathBuf;

use super::super::{flags, Cpu, FlatBus};

/// Where the fetched vectors live.
fn vectors_dir() -> PathBuf {
    std::env::var_os("RF_65816_VECTORS").map_or_else(
        || {
            PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .parent()
                .and_then(|p| p.parent())
                .expect("crates/rf-snes is two levels under the repo root")
                .join("roms/snes/singlestep-65816/v1")
        },
        PathBuf::from,
    )
}

/// One `initial`/`final` block.
#[derive(Default, Clone, PartialEq, Eq)]
struct State {
    pc: u16,
    s: u16,
    p: u8,
    a: u16,
    x: u16,
    y: u16,
    dbr: u8,
    d: u16,
    pbr: u8,
    e: u8,
    ram: Vec<(u32, u8)>,
}

/// A hand-rolled reader for exactly this schema.
///
/// The workspace has no JSON dependency and adding one for a test would
/// need a `docs/TECH_STACK.md` row and a licence review — the same
/// reasoning `rf_nes::cpu::tests::json` records, and the same answer.
/// This is not a general JSON parser and does not try to be.
struct Cursor<'a> {
    b: &'a [u8],
    i: usize,
}

impl<'a> Cursor<'a> {
    fn new(b: &'a [u8]) -> Self {
        Self { b, i: 0 }
    }

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
        assert!(self.eat(c), "expected {:?} at byte {}", c as char, self.i);
    }

    /// Read a bare key name out of `"key":`.
    fn key(&mut self) -> &'a str {
        self.skip_ws();
        self.expect(b'"');
        let start = self.i;
        while self.b[self.i] != b'"' {
            self.i += 1;
        }
        let k = std::str::from_utf8(&self.b[start..self.i]).expect("ascii key");
        self.i += 1;
        self.expect(b':');
        k
    }

    fn number(&mut self) -> i64 {
        self.skip_ws();
        let start = self.i;
        if self.b[self.i] == b'-' {
            self.i += 1;
        }
        while self.i < self.b.len() && self.b[self.i].is_ascii_digit() {
            self.i += 1;
        }
        std::str::from_utf8(&self.b[start..self.i])
            .expect("ascii number")
            .parse()
            .expect("integer")
    }

    /// Skip one value of any type, used for the `cycles` array this
    /// ticket does not compare.
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
            b'[' | b'{' => {
                let (open, close) = if self.b[self.i] == b'[' {
                    (b'[', b']')
                } else {
                    (b'{', b'}')
                };
                let mut depth = 0;
                loop {
                    let c = self.b[self.i];
                    if c == b'"' {
                        self.i += 1;
                        while self.b[self.i] != b'"' {
                            self.i += 1;
                        }
                    } else if c == open {
                        depth += 1;
                    } else if c == close {
                        depth -= 1;
                        if depth == 0 {
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
        match k {
            "ram" => {
                cur.expect(b'[');
                if !cur.eat(b']') {
                    loop {
                        cur.expect(b'[');
                        let addr = cur.number();
                        cur.expect(b',');
                        let value = cur.number();
                        cur.expect(b']');
                        st.ram.push((
                            u32::try_from(addr).expect("24-bit address"),
                            u8::try_from(value).expect("byte"),
                        ));
                        if !cur.eat(b',') {
                            break;
                        }
                    }
                    cur.expect(b']');
                }
            }
            _ => {
                let v = cur.number();
                match k {
                    "pc" => st.pc = v as u16,
                    "s" => st.s = v as u16,
                    "p" => st.p = v as u8,
                    "a" => st.a = v as u16,
                    "x" => st.x = v as u16,
                    "y" => st.y = v as u16,
                    "dbr" => st.dbr = v as u8,
                    "d" => st.d = v as u16,
                    "pbr" => st.pbr = v as u8,
                    "e" => st.e = v as u8,
                    other => panic!("unknown state key {other}"),
                }
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

fn parse_file(bytes: &[u8]) -> Vec<Vector> {
    let mut cur = Cursor::new(bytes);
    let mut out = Vec::new();
    cur.expect(b'[');
    if cur.eat(b']') {
        return out;
    }
    loop {
        cur.expect(b'{');
        let mut name = String::new();
        let mut initial = State::default();
        let mut final_ = State::default();
        loop {
            let k = cur.key();
            match k {
                "name" => {
                    cur.skip_ws();
                    cur.expect(b'"');
                    let start = cur.i;
                    while cur.b[cur.i] != b'"' {
                        cur.i += 1;
                    }
                    name = String::from_utf8_lossy(&cur.b[start..cur.i]).into_owned();
                    cur.i += 1;
                }
                "initial" => initial = parse_state(&mut cur),
                "final" => final_ = parse_state(&mut cur),
                // `cycles` is skipped — see the module doc.
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

fn cpu_from(state: &State) -> Cpu {
    Cpu {
        a: state.a,
        x: state.x,
        y: state.y,
        sp: state.s,
        d: state.d,
        dbr: state.dbr,
        pbr: state.pbr,
        pc: state.pc,
        p: state.p,
        e: state.e != 0,
        stopped: false,
    }
}

/// Run one vector. Returns a description of the first mismatch.
fn run_one(v: &Vector) -> Result<(), String> {
    let mut cpu = cpu_from(&v.initial);
    let mut bus = FlatBus::new();
    for (addr, value) in &v.initial.ram {
        bus.mem[*addr as usize] = *value;
    }

    cpu.step(&mut bus)
        .map_err(|op| format!("unimplemented opcode {op:#04X}"))?;

    let mut wrong = Vec::new();
    let mut check = |name: &str, got: u64, want: u64| {
        if got != want {
            wrong.push(format!("{name}: got {got:#06X}, want {want:#06X}"));
        }
    };
    check("pc", cpu.pc.into(), v.final_.pc.into());
    check("a", cpu.a.into(), v.final_.a.into());
    check("x", cpu.x.into(), v.final_.x.into());
    check("y", cpu.y.into(), v.final_.y.into());
    check("s", cpu.sp.into(), v.final_.s.into());
    check("d", cpu.d.into(), v.final_.d.into());
    check("p", cpu.p.into(), v.final_.p.into());
    check("dbr", cpu.dbr.into(), v.final_.dbr.into());
    check("pbr", cpu.pbr.into(), v.final_.pbr.into());
    check("e", u64::from(cpu.e), v.final_.e.into());
    for (addr, want) in &v.final_.ram {
        let got = bus.mem[*addr as usize];
        if got != *want {
            wrong.push(format!(
                "ram[{addr:#08X}]: got {got:#04X}, want {want:#04X}"
            ));
        }
    }

    if wrong.is_empty() {
        Ok(())
    } else {
        Err(wrong.join("; "))
    }
}

/// Run every case in one opcode/mode file.
fn run_file(path: &std::path::Path) -> (usize, usize, Vec<String>, bool) {
    let bytes = std::fs::read(path).expect("vector file readable");
    let vectors = parse_file(&bytes);
    let (mut pass, mut fail) = (0, 0);
    let mut failures = Vec::new();
    let mut unimplemented = 0usize;
    for v in &vectors {
        match run_one(v) {
            Ok(()) => pass += 1,
            Err(why) => {
                fail += 1;
                if why.starts_with("unimplemented opcode") {
                    unimplemented += 1;
                }
                if failures.len() < 3 {
                    failures.push(format!("{}: {why}", v.name));
                }
            }
        }
    }
    let all_unimplemented = !vectors.is_empty() && unimplemented == vectors.len();
    (pass, fail, failures, all_unimplemented)
}

/// Opcodes excluded from the suite, with the reason for each.
///
/// **Excluded, not waived** — the same distinction, and deliberately the
/// same wording, the nes6502 evidence file draws for `$AB`. A waiver says
/// "this is broken and we accept it". An exclusion says "this vector
/// cannot be evaluated by this runner at all". Every entry below is the
/// second kind and names the ticket that will remove it.
const EXCLUDED: &[(u8, &str)] = &[
    (
        0x00,
        "BRK: interrupt dispatch is W6-01b, not this ticket. `step` returns Err(opcode) rather \
         than guessing at a vector fetch it does not implement (see the cpu module doc).",
    ),
    (
        0x44,
        "MVP: cycle-truncated mid-instruction, exactly as $54 — see there.",
    ),
    (
        0x54,
        "MVN: SingleStepTests caps each case at a fixed cycle count and a block move runs past \
         it. Verified on `54 e 1`: 100 cycles is 14 complete 7-cycle iterations plus 2 cycles \
         of a 15th, leaving final PC two bytes into the operand and A/X/Y mid-move. No runner \
         that steps whole instructions can reproduce a state captured 2 cycles into one; that \
         needs the master-cycle model, which is W6-01b.",
    ),
];

/// **The acceptance criterion.** Every fetched opcode file, both modes,
/// every case, minus the recorded exclusions above.
///
/// `#[ignore]`d: 2.78M cases over gigabytes of local-only data, exactly
/// like the nes6502 suite. Run with
///   cargo test --release -p rf-snes -- --ignored --nocapture
#[test]
#[ignore = "local vector suite; fetch with scripts/fetch-65816-vectors.sh"]
fn singlestep_65816_vectors() {
    let dir = vectors_dir();
    let Ok(entries) = std::fs::read_dir(&dir) else {
        eprintln!(
            "SKIP: no 65816 vectors at {} — run scripts/fetch-65816-vectors.sh",
            dir.display()
        );
        return;
    };
    let mut files: Vec<PathBuf> = entries
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|x| x == "json"))
        .collect();
    files.sort();

    assert!(
        !files.is_empty(),
        "the vectors directory exists but is empty — a suite that ran zero cases is not a pass"
    );

    let (mut total_pass, mut total_fail) = (0usize, 0usize);
    let mut report = Vec::new();
    let mut modes = std::collections::BTreeSet::new();
    let mut unimplemented = std::collections::BTreeSet::new();
    let mut covered = std::collections::BTreeSet::new();
    let mut excluded_files = 0usize;

    for path in &files {
        let stem = path
            .file_stem()
            .unwrap_or_default()
            .to_string_lossy()
            .into_owned();
        let mut parts = stem.split('.');
        let opcode = parts
            .next()
            .and_then(|o| u8::from_str_radix(o, 16).ok())
            .unwrap_or_else(|| panic!("unparseable vector filename {stem}"));
        if let Some(mode) = parts.next() {
            modes.insert(mode.to_string());
        }
        if EXCLUDED.iter().any(|(op, _)| *op == opcode) {
            excluded_files += 1;
            continue;
        }

        let (pass, fail, failures, all_unimplemented) = run_file(path);
        if all_unimplemented {
            // The core does not implement this opcode at all. That is a
            // COVERAGE fact, not a failure: this ticket builds a subset of
            // the instruction set on purpose, and the gate's job is to
            // police what the core claims to do. It is reported loudly
            // below so the gap cannot go unnoticed.
            unimplemented.insert(opcode);
            continue;
        }
        covered.insert(opcode);
        total_pass += pass;
        total_fail += fail;
        if fail > 0 {
            report.push(format!(
                "{stem}: {fail} failed of {}\n    {}",
                pass + fail,
                failures.join("\n    ")
            ));
        }
    }

    // Coverage, stated out loud rather than implied by a green run.
    //
    // The fetch script takes all 256 opcodes and does NOT derive its list
    // from the implementation. That matters: the first version of this
    // suite built its opcode list by grepping `ops.rs`, which made the
    // gate self-referential — it could not report a gap it had not
    // already been told about — and a completely broken `alu_mode` offset
    // table sat underneath an all-green 139-of-139 report because of it.
    let unimpl_list: Vec<String> = unimplemented.iter().map(|o| format!("{o:02X}")).collect();
    eprintln!(
        "65816 vectors: {total_pass} passed, {total_fail} failed ({} cases)",
        total_pass + total_fail
    );
    eprintln!(
        "  coverage: {} of 256 opcodes implemented and tested; {} not implemented; \
         {excluded_files} files excluded",
        covered.len(),
        unimplemented.len()
    );
    eprintln!(
        "  not implemented: {}",
        if unimpl_list.is_empty() {
            "none".to_string()
        } else {
            unimpl_list.join(" ")
        }
    );
    for (op, why) in EXCLUDED {
        eprintln!("  excluded ${op:02X}: {why}");
    }

    // The unimplemented set is PINNED, not merely reported. Printing it
    // tells a reader what the gap is; asserting it means the gap cannot
    // quietly widen — deleting an opcode's implementation would otherwise
    // just make this suite print a slightly longer line and still pass.
    //
    // All four are interrupt-coupled and belong to W6-01b with $00:
    //   $02 COP — software interrupt, same vector machinery as BRK
    //   $40 RTI — returns from one
    //   $CB WAI — waits for one
    //   $DB STP — halts until reset, the degenerate case of the same
    const EXPECTED_UNIMPLEMENTED: &[u8] = &[0x02, 0x40, 0xCB, 0xDB];
    let actual: Vec<u8> = unimplemented.iter().copied().collect();
    assert_eq!(
        actual, EXPECTED_UNIMPLEMENTED,
        "the set of unimplemented opcodes changed; update EXPECTED_UNIMPLEMENTED \
         (and this ticket's scope note) deliberately rather than by accident"
    );

    // Both halves of every opcode. The acceptance says "incl. m/x width
    // handling", and emulation mode is exactly where those flags are
    // forced — a run that only covered native mode would miss the half
    // this ticket is most about.
    assert!(
        modes.contains("e") && modes.contains("n"),
        "both emulation (.e) and native (.n) files must be present; found {modes:?}"
    );

    assert!(
        total_fail == 0,
        "{total_fail} vector case(s) failed:\n{}",
        report.join("\n")
    );
}

/// Anti-vacuity for the suite above: prove the harness can FAIL.
///
/// A runner with a broken comparison passes everything, and "0 failed"
/// looks identical whether the CPU is right or the check is dead. This
/// feeds it a vector whose expected state is deliberately wrong and
/// requires a mismatch.
#[test]
fn the_vector_harness_reports_a_mismatch_rather_than_passing_everything() {
    let v = Vector {
        name: "synthetic".to_string(),
        initial: State {
            pc: 0x8000,
            s: 0x01FF,
            p: flags::M | flags::X,
            e: 1,
            ram: vec![(0x0000_8000, 0xEA)], // NOP
            ..State::default()
        },
        final_: State {
            // A NOP cannot change A. Requiring that it does must fail.
            pc: 0x8001,
            s: 0x01FF,
            p: flags::M | flags::X,
            a: 0x1234,
            e: 1,
            ram: vec![(0x0000_8000, 0xEA)],
            ..State::default()
        },
    };
    let err = run_one(&v).expect_err("a wrong expectation must be reported");
    assert!(
        err.contains("a:"),
        "the mismatch must name the register: {err}"
    );
}
