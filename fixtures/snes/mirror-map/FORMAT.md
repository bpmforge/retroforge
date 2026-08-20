# SNES mirror-map fixture

Two cartridge images built from **one source** (`src/main.s`) with a `-D`
switch, so they differ in the mapping under test and nothing else
(ticket W6-06; FR-CORE-035, `docs/TESTING.md` §5's
"libSFX-built LoROM/HiROM mirror-map fixtures" row).

| image | size | map mode | header at | canonical bank |
|---|---|---|---|---|
| `build/lorom.sfc` | 32 KiB | `$20` | `$7FC0` in file | `$80` |
| `build/hirom.sfc` | 64 KiB | `$21` | `$FFC0` in file | `$C0` |

Build both, with hash pinning, via `./build.sh`. Hashes live in
`rom-lorom.sha256` / `rom-hirom.sha256`; the script refuses a mismatch
rather than re-pinning silently.

## Toolchain: cc65, not libSFX — and why that is not a shortcut

D-001 and W6-06 both said "libSFX", and the phase-entry assumption was
that a new toolchain had to be installed and licence-checked before any
SNES work could start. Checked rather than assumed: **cc65's `ca65`
already assembles 65816** (`ca65 --cpu 65816`, with `.p816` and explicit
`.a8`/`.a16` width directives), and `ld65` links a flat LoROM or HiROM
image from a linker config. cc65 is **already installed in CI** for the
NES fixture.

So these fixtures need no new dependency, no `docs/TECH_STACK.md` row and
no licence review, and the SNES phase is not gated on a toolchain
decision at all.

libSFX remains the right answer for **RF-Scroller-S** (W6-05), which wants
a framework's init code, macros and asset pipeline. A *cartridge mapping*
test needs one bank, a header and vectors; pulling in a framework for that
would be a large dependency contributing nothing the test uses.

## What the ROMs check

Four checks, written as a result block in WRAM at `$7E:0000`:

| offset | meaning |
|---|---|
| `$00`-`$01` | magic `'R'`, `'F'` — **written last** |
| `$02` | number of checks run |
| `$03` | number passed |
| `$04`+ | one byte per check, 1 = pass |

0. The ROM is readable through the mapping's canonical bank.
1. The bus mirrors the ROM into the low banks — `$00:8000`↔`$80:8000` for
   LoROM, `$40:8000`↔`$C0:8000` for HiROM. Same question, asked at the
   address each mapping actually uses.
2. WRAM `$7E:0000-1FFF` is mirrored into `$00:0000-1FFF`: write through
   one view, read through the other.
3. That write did not alias into ROM space — re-read check 0's byte.

**The magic is written last on purpose.** A harness that finds it present
knows every byte beneath it came from a run that reached the end, rather
than reading uninitialised WRAM that happened to look plausible. The block
is also zeroed first, so a check that never ran reads 0 (fail) rather than
whatever powered up there.

The ROMs deliberately touch no PPU, APU, DMA or interrupts: a mapping test
that needed a working PPU could not run until the PPU worked, which is
backwards for the first SNES fixture in the project.

## Status: these now run

**Closed by W6-02a.** `rf-snes` has a bus, so both images boot on it and
their result blocks are read and asserted by
`crates/rf-snes/tests/mirror_map_fixtures.rs`: magic present, 4 checks
run, 4 passed. The structural test in
`crates/rf-harness/tests/snes_mirror_map_fixtures.rs` still stands and is
still worth having — it catches build non-determinism and header damage,
which executing the ROM does not.

### And running them found a bug in check 1

Worth recording, because it is the exact failure "assert it from inside
the machine" exists to catch, and because it survived a structural test
that looked thorough.

The LoROM branch was written `lda $008000` / `cmp $808000`. `ca65` saw a
bank of `$00` on the first operand, shortened it to a 2-byte **absolute**
`ad 00 80`, and absolute addressing resolves through `DBR` — which this
ROM sets to `$7E` for its result-block stores. So check 1 compared WRAM
`$7E:8000` against ROM `$80:8000`. It could only ever fail, and had it
"passed" it would have proved nothing about the alias it names.

HiROM never showed the problem: its operands are banks `$40`/`$C0`, so
`ca65` had no short form to pick and emitted long addressing for both.
An asymmetry between the two images coming from the **assembler** rather
than the mapping is precisely what "one source, two mappings" is meant to
rule out.

The fix is `f:` on both operands, forcing 24-bit addressing. If you add a
check here that names a specific bank, write `f:` and mean it — and note
that rebuilding changed only `lorom.sfc`; `hirom.sfc` rebuilt
byte-identical to its existing pin, which is how the fix was confirmed to
be LoROM-only.

## One thing that went wrong, worth not repeating

The first build put the header at `$FFB0`. That is the **extended**
header; the cartridge header a console reads is at `$FFC0`. The ROM
assembled, linked and looked fine, and reported its title as `"XTURE"` —
the real title read five bytes into itself. If a SNES image looks
structurally valid but its title is garbage, check the header offset
before anything else.
