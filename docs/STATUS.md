# STATUS ledger — one line per merged ticket (see PLAYBOOK.md)

2026-07-06 W0-01 done — repo bootstrap: 16-crate workspace, CI, arch validator, SDLC docs, plan.json (39 tickets)

## 2026-07-15 — DESIGN-REVIEW GATE (arc P6): all passes complete, validators green, challenger recorded

**Arc:** P0 preflight → P1 review (44-gap register, docs/work/DESIGN_REVIEW.md) → P2 interrogations (R-x index) → Brad: fixture question → D-001 self-contained fixtures → Brad: "adopt all" → D-002..D-007 → P3 threading → P4 validators/ADRs/FAILURE_MODES/CONTRACTS (Ralph converged iteration 2) → P5 board completion (11 splits, stories linkage, hold flags) + fresh-agent challenger (17 findings, all closed same-day) → this gate. Board 39→64 tickets / 316→366 pts. Branch `review/design-review-hardening`, both remotes.

**Gate evidence (independent re-runs, this session):**
- re-ran independently: `node scripts/validate-plan.mjs` — 64 tickets · 366 pts · P1–P9 incl. WIP=1 territory law · claimable W0-02 W0-04 · held W5-04 W5-05 — OK, exit 0
- re-ran independently: `node scripts/validate-traceability.mjs` — 100/100 FR/NFR reachable · 33/33 stories covered (10 planning-ticket-only, listed) · 8 decisions · zero dangling refs — OK, exit 0
- re-ran independently: `cargo fmt --check && cargo clippy --workspace -- -D warnings && cargo test --workspace && scripts/validate-arch.sh` — pinned 1.94 toolchain, 31 suites, arch OK — exit 0
- Challenger verdicts: validators/counts/split-integrity/done-immutability CONFIRMED; ownership claim CONTRADICTED with ~17 findings (Alter Ego half-owned incl. a reviewer-introduced misquote of D-001; breakpoint engine, trace/audio viewers, FM-01/FM-13, 5 all-of-the-set failures, 2 hollow links) — all closed same-day (commit faadf90); re-verified green after closure.

**Accepted residuals (listed, never silently waived):**
- 10 late-phase stories (E8-S2, E9-S1/S2, E10-*, E11-S1, E12-*) covered only by planning tickets W7-01/W8-01/W9-01 — real coverage lands at phase-entry expansion; the validator now reports this class explicitly.
- NFR-007 SAFETY-comment enforcement rides workspace lint config (clippy undocumented_unsafe_blocks) + W1-01a acceptance, not a dedicated ticket.
- docs/work/telemetry.jsonl's 2026-07-07 validate-ux-spec failure is unknowable (validators never existed in-repo) — superseded by the in-repo suite (G-36).
- 247+ todo-overlap write-scope pairs are legal under WIP=1 (P8 informational count); revisit only if parallel executors are ever introduced.
- Alter Ego PD status is an informal source-zip claim (RISKS R-16 residual).

**Board:** 64 tickets / 366 pts · statuses 1 done / 63 todo · vetoable decisions D-001..D-006 (Brad may veto any individually).

2026-07-15 review arc MERGED to main (92cc3ce), both remotes; validators re-verified green post-merge. Resume point for implementation sessions: docs/work/HANDOFF.md (claimable: W0-02, W0-04).

2026-08-02 W0-02 done — rf-cart iNES/NES2.0/SNES parsing + RA-convention normalized CRC32/MD5/SHA-1/SHA-256 (+raw hash for diagnostics); typed CartError, no panics on malformed input. workspace: 47 passing (rf-cart 33). Gate re-run independently by conductor: fmt/clippy(--all-targets)/test/arch/plan/traceability all exit 0. Deps added: sha2+sha1+md-5 0.11, crc32fast 1.5 (TECH_STACK §2 row updated; RustCrypto 0.11 drops LowerHex on digest output — hex shim in hash.rs).

2026-08-02 W0-04 done — rf-core-api traits v0: EmulatorCore/CoreSink/StateView/CoreEvent/InputFrame/Step per ARCHITECTURE §5; indexed PpuPixel carries palette_index+layer+sprite_id+priority+dropped_by_limit (full FR-CORE-004 set, incl. the flag W3-05 consumes); EventMask is a dep-free u32 newtype with const contains. ZERO dependencies. Cartridge layering resolved by a local CartImage<'a> borrow (rf-core-api may not depend on any rf-* crate); event_mask exposed via existing CoreConfig rather than a new trait method, so the §5 method set is unchanged. workspace: 61 passing (rf-core-api 15). Gate re-run independently by conductor: all six exit 0.

2026-08-02 W0-03 done — rf-harness: manifest fetcher (mirror-list schema per FM-14, sha256 on artifact not per-mirror), blargg $6000/$6004 protocol vs rf-core-api traits + mock (no core exists yet, by design), golden-frame hash helper, accuracy-table JSON with raw/effective split and waiver expiry (injected `today`, no SystemTime). workspace: 114 passing (rf-harness 54). Gate re-run independently by conductor: all six exit 0. NFR-006 verified: no binaries added, /roms/ gitignored, zero ROM files tracked. Residuals: Holy Diver Batman + blargg spc_timing have no manifest entry (blocked-with-evidence in manifest header); 65816 + PeterLemon carry honest TODO- placeholders, never fabricated hashes; default fetch pulls a 446MB archive — size this before CI wiring.

2026-08-02 W0-05 done — rf-state .rfstate TLV container: RFST header (magic/version/console/flags/rom_sha256/emu_version/timestamp) + zstd chunk body, bincode 2 payloads, tag registry with required-core-set enforcement. workspace: 151 passing (rf-state 34), 1 ignored (fixture regenerator, by design). Gate re-run independently by conductor: all six exit 0. Hardening beyond ticket: decompression-bomb cap at 64 MiB (wire format carries no decompressed-size field) and oversized-len rejection before allocation, both tested. Determinism trap closed: timestamp/emu_version excluded from state_hash, proven by a test that also asserts bytes DIFFER so the hash-equality cannot pass vacuously. FOLLOW-UP: SAVE_STATES.md §2 now records the two wire encodings W0-05 had to choose (emu_version u16-length-prefix cap 128; timestamp u64 LE Unix-seconds) — public commitments under NFR-008.

2026-08-02 W0-06 done — cargo-deny licence gate (NFR-011): deny.toml allowlist (9 SPDX ids), zstd-sys dual BSD-3/GPL-2 elected to BSD-3 via [[licenses.clarify]] with a source-computed XxHash32 license-file hash, THIRD-PARTY-NOTICES at root, CI step pinned to cargo-deny 0.20.2 (in-job cargo install, NOT the Embark Docker action — its musl image would run an untested rustup resolution against the 1.94 pin). Tree licences today: MIT (55), Apache-2.0 (50), BSD-3-Clause (1, elected), Unicode-3.0 (1, per-crate exception). Gate proven non-vacuous INDEPENDENTLY by conductor: removing MIT+Apache-2.0 from the allowlist yields exit 4 with 161 rejections. workspace: 151 passing, unchanged (no crates touched). OPEN: NFR-011 does not enumerate Unicode-3.0 (required transitively by unicode-ident via serde_derive); scoped as a per-crate exception rather than silently widening the allowlist — needs Brad to amend or ratify.
