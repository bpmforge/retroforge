# HANDOFF — resume point for the next session (rewritten 2026-08-03)

For a fresh coding session (any model). The design-review arc is DONE and
merged. **Your job is implementation: execute tickets from plan.json, one at
a time.** Do not redesign anything.

## Read in this order (skim, don't study)

1. `CLAUDE.md` — the laws (short, mandatory)
2. `MASTER_PROMPT.md` — session protocol incl. the **always-writable set**
3. `plan.json` — the board (65 tickets; schema note at top is authoritative)
4. `PLAYBOOK.md` — per-ticket loop, gate command, block-note discipline
5. The tail of `docs/STATUS.md` — per-ticket evidence for everything below

## Current state (verified 2026-08-03, commit 0eb2b69 on main, both remotes)

**8 tickets done. Workspace tests: 162 passing** (with the 1 GB vector set
absent — see "Large test data" below).

| Ticket | What landed |
|---|---|
| W0-01 | CI pipeline + arch validator |
| W0-02 | rf-cart: iNES/NES2.0/SNES parse, RA-convention normalized hashing |
| W0-03 | rf-harness: manifest fetcher, blargg $6000/$6004 protocol, accuracy table + waivers |
| W0-04 | rf-core-api: EmulatorCore/CoreSink/StateView/CoreEvent/PpuPixel/EventMask, zero deps |
| W0-05 | rf-state: `.rfstate` TLV container + zstd + bincode 2 |
| W0-06 | cargo-deny licence gate (NFR-011) |
| W1-01a | 6502: 151 official opcodes, cycle-stepped |
| W1-01b | 6502: 105 unofficial opcodes + interrupt edges |

**The 6502 is feature-complete for Phase 1**: all 256 opcodes pass
SingleStepTests `nes6502` — **2,560,000/2,560,000 cases**, state + RAM +
cycle-by-cycle bus trace. Zero `unsafe` in rf-nes.

Full gate GREEN:
`cargo fmt --check && cargo clippy --workspace -- -D warnings && cargo test --workspace && scripts/validate-arch.sh && node scripts/validate-plan.mjs && node scripts/validate-traceability.mjs`

Toolchain pinned **1.94** (rust-toolchain.toml — never change to "stable").
Board: 65 tickets / 369 pts · validators green · traceability 101/101.

## START HERE — recommended order

1. **W0-07** — local evidence gate + manifest git-artifact kind. **Not yet on
   the board**; a schema-checked draft exists (see "Open items"). Should land
   before Phase 1 exit depends on it.
2. **W1-02** — bus + NROM + DMA. Claimable now. This is where the DMC-DMA
   seam gets proven (see "Known risk").
3. **W1-03** — nestest golden trace; consumes W1-01b's `Cpu::unstable_op`
   marker.

Claim = set `in_progress` in plan.json + commit that change first.

## Execution pattern that has been working (D-003 "one conductor")

Conductor in the main session; **Sonnet** subagents implement ONE ticket each,
**strictly serial** (WIP=1 is law; tickets share Cargo.lock and target/, so
parallel gates collide). The conductor owns all status mutation: claim before
spawning, set `done` only after an **independent** six-command gate re-run.
Never accept a subagent's "gate is green" — RF-L-08 is this project's own
lesson that a green report hid 17 real gaps. That has already paid off
repeatedly.

**No auto-frontier**: conductor fixes only mechanical nits (fmt/clippy).
Semantic failures get one Sonnet retry with evidence pasted in, then park
blocked-with-evidence for the human queue.

**Pre-flight every ticket**: verify external crate APIs by compile-probing in
a scratchpad `cargo new` BEFORE writing the brief. This caught two traps that
would each have burned a session (below).

## Traps already paid for — do not rediscover

- **plan.json formatting**: 1-space indent AND `\uXXXX` escaping for non-ASCII
  (§, —, ±). A `JSON.parse`→`JSON.stringify` round-trip reformats all 1474
  lines and unescapes them. **Always edit plan.json with a surgical text
  edit.**
- **RustCrypto 0.11** (sha2/sha1/md-5): `Digest::finalize()` returns
  `Array<u8,N>` which does NOT impl `LowerHex`. The 0.10-era
  `format!("{:x}", …)` is compile error E0277. See `crates/rf-cart/src/hash.rs`.
- **`bincode 3.0.0` is NOT a release** — bincode-org published it as a
  placeholder whose entire lib.rs is `compile_error!("https://xkcd.com/2347/")`.
  Use **bincode 2**. Do not "upgrade".
- **cargo-deny 0.20.x** `[licenses]` is allow-list-only; the old
  `unlicensed`/`copyleft`/`deny` keys were REMOVED in 0.14 and now hard-error.
- **validate-traceability ordering**: F2 (ticket cites undefined FR/NFR) is a
  HARD failure; W1 (FR/NFR no ticket cites) is only a warning. So an SRS row
  must land in the same commit as — or before — the ticket citing it.
- **Board defect pattern**: some tickets have a `write_scope` that makes their
  own acceptance impossible. W1-01a's did (fixed 2026-08-03 by adding the
  exact paths, not widening the glob — Brad's ruling). **W1-04a and W1-05a
  have the same defect with `ppu/**`** and need `crates/rf-nes/src/lib.rs`
  added for `mod ppu;` wiring. Check scope-vs-acceptance BEFORE claiming.

## Large test data (shapes every CPU/PPU ticket)

The `nes6502` vectors (~1.0 GB, 256 files) are already extracted at
`roms/nes/singlestep-nes6502-src/nes6502/v1/` — **gitignored, never commit**
(NFR-006). The harness finds them via `RF_NES6502_VECTORS` or that default
path.

**`cargo test --workspace` MUST pass when the vectors are ABSENT** (skip
cleanly). CI and any fresh machine won't have them. But that skip path is also
the obvious way to fake success — always require real per-opcode counts as
evidence, and re-run them yourself.

**Broken manifest entry**: `singlestep-nes6502`'s pinned sha256 does not match
what codeload serves. Root cause (verified): codeload sends **no
`Content-Length`** and streams, so a multi-GB transfer truncates silently into
a valid-looking file. codeload IS byte-stable per commit — `singlestep-spc700`
(15 MB) hashes exactly right — so this is one broken entry, not a systemic
format problem. Fix in W0-07: use a partial+sparse **git clone at a pinned
commit** (a commit SHA is itself a content hash), not an archive sha256.

## Known risk for W1-02

`Cpu::step` is **instruction-granular** — it issues every bus cycle in exact
hardware order, but steps one instruction per call. Per-cycle PPU/APU
interleaving is intended to come from the `CpuBus` impl ticking chips inside
`read`/`write`. Conductor analysis says this **holds**: the bus owns the master
clock, so it can insert DMC stall cycles and perform the `$2007`/`$4016` read
twice internally to produce the RDY double-read side effect, returning the
second value. Verify that in practice early in W1-02 — if it doesn't hold,
W1-02 becomes a rewrite rather than an addition.

## Open items needing Brad

- Evidence file format for W0-07: single **rolling** file (recommended) vs
  per-run files.
- **Holy Diver Batman contradiction**: DESIGN_REVIEW §4 records it as
  pinobatch/holy-mapperel; W0-03's research says that is a different project
  (PCB manufacturing test). One is wrong. Tier-B/nightly, not blocking.
- HP-4..7 in `docs/PREREQUISITES.md` still open. **HP-4 (GitHub Actions
  budget) is now known-constrained** — Brad is Actions-limited, which is why
  W0-07 moves heavy verification local with a staleness-checked evidence file
  rather than adding CI cost.
- Vetoes on D-001..D-006 remain open.
- W5-04 and W5-05 are `hold: true` — humans only, never claim.

## Rules a cheap model must not improvise around

1. ONE ticket per session. Stay inside `write_scope` + the always-writable set
   (plan.json own status/notes · docs/STATUS.md append · Cargo.lock ·
   docs/TECH_STACK.md §2 row for any new dependency).
2. Verify EVERY external crate API against docs.rs or
   `~/.cargo/registry/src/` for the pinned version before use.
3. Full gate must pass before marking a ticket done. Then append one line to
   docs/STATUS.md and commit as `feat(W1-02): <summary>`.
4. Blocked? Write a `notes` entry: root cause · exact fix · why workarounds
   fail · "do not retry without X". Two blocked attempts = stop, leave for a
   human. Blocked-with-evidence is success, not failure.
5. Determinism invariant: no wall-clock/RNG/floats in core crates
   (validate-arch.sh greps rf-core-api/rf-nes/rf-snes/rf-cart). Cores emit
   indexed pixels, never RGB.
6. No ROM bytes in git.
7. Push after merged work: `git push origin main && git push github main`.
8. Commit trailer names the implementing model.
