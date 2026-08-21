# Design: Community distribution format

Ticket **W9-05**. Implements **FR-PROF-005/006/007** and ROADMAP P9's
"community-safe: no ROM data, license metadata required". `CLAUDE.md`
law 5 is the project-side counterpart of the same rule.

Code: `crates/rf-enhance/src/distribution.rs`, binary
`crates/rf-enhance/src/bin/rf-intake.rs`.

## 1. What a bundle is

```text
<bundle>/
  bundle.toml     flat `key = value`: id, version, license, provenance
  index           one `<kind> <path>` line per member
  ...members...
```

Both metadata files are hand-parsed, the same deliberate choice
`rf_plugin_sdk::manifest` made: the format is flat, this must not drag a
parser dependency into `rf-enhance`, and a checker that cannot itself fail
in interesting ways is worth more than one supporting nesting nobody uses.

## 2. The allowlist is the mechanism; the ROM sniffer is the diagnostic

**This is the load-bearing decision, and the obvious design gets it
backwards.**

A **denylist** of ROM signatures cannot work:

- renaming `game.nes` to `data.bin` defeats an extension check;
- a headerless dump — which is most SNES ROMs — defeats a magic-byte
  check.

Any sniffer filters things that *look like* ROMs, and "looks like" is not
a property anyone can enumerate. So the format is **deny-by-default over
member kinds**. A bundle may carry exactly:

| Kind | What it is |
|---|---|
| `profile` | a `profile.toml` (FR-PROF-005/006) |
| `pack-manifest` | a replacement-pack manifest (`hires.txt` or ours) |
| `pack-image` | a replacement image a manifest references |
| `mod-patch` | a `[mods.patch]` declaration — facts, never ROM bytes |
| `license` | the bundle's licence text |
| `readme` | human-readable documentation |

There is deliberately **no `other`/`data` variant**, because one would
reopen exactly the hole this closes. An arbitrary blob is refused for
being *unclassifiable*, not for resembling a ROM — a property the format
enforces structurally rather than a pattern it hopes to recognise.

`sniff_rom` exists so the *diagnostic* is good: someone who accidentally
zipped their ROM in should be told "this looks like an iNES ROM", not
"unknown member kind". It also catches the case the allowlist cannot — a
ROM **labelled** as a legitimate `pack-image` — so both run, on every
member.

### What this cannot do, stated plainly

It **cannot prove a bundle contains no ROM data.** A determined submitter
can base64 a ROM into a readme and nothing here would notice.

`IntakeReport::summary()` therefore states exactly what a pass means — no
member declared an uncarried kind, and nothing matched a ROM signature —
and says explicitly that this is *not* proof the bundle is free of game
data. A test asserts that wording. An "ACCEPTED" that read as "verified
ROM-free" would be the same species of overclaim the project refuses
everywhere else.

## 3. Licence metadata

FR-PROF-007: "SPDX license + provenance metadata required; missing/unknown
licenses and denylisted content (NC assets, GFDL text, GPL-derived shader
code) rejected; nothing activates without passing."

Three outcomes, and the split between the last two is the half that is
easy to get wrong:

| Declared | Result |
|---|---|
| absent | `MissingLicense` |
| on `ALLOWED_LICENSES` | accepted |
| recognised and unacceptable (NC, ND, GFDL, GPL/AGPL/LGPL, SSPL/BUSL) | `DeniedLicense`, **with the reason** |
| anything else | `UnknownLicense` — **refused, not assumed permissive** |

Treating "I don't know this string" as acceptable is how a non-commercial
asset ships from a project whose entire licence policy is allowlist-only
(NFR-011). And a submitter who wrote `CC-BY-NC-4.0` is told *why* it
fails rather than that nobody has heard of it — the two are different
errors and prompt different fixes.

`ALLOWED_LICENSES` mirrors `deny.toml`'s allowlist plus the asset licences
(`CC-BY-4.0`, `CC-BY-SA-4.0`) that a code allowlist has no reason to
carry.

## 4. Intake reports every fault at once

`intake()` collects rather than returning the first problem, the same rule
the pack validator follows: a submitter fixes their bundle in one CI run
instead of one fault per run.

## 5. The CI check

`rf-intake <bundle-dir>` exits **0 accepted, 1 refused, 2 unreadable**.

The third code is not pedantry. A malformed checkout reported as a policy
failure sends a submitter to fix a licence that was never the problem, so
"could not read the bundle" is a distinct outcome from "the bundle is not
compliant". Correspondingly, a member listed in `index` but absent on disk
is a **read** failure, while a *missing metadata key* becomes an empty
string and is reported by `intake` as the policy fault it actually is.

## 6. What this ticket does not do

- **No `.github/workflows` wiring.** `rf-intake` exists and its exit codes
  are the contract, but nothing invokes it in CI yet —
  `.github/**` is outside W9-05's write_scope. Until that lands, the
  check is available rather than enforced, and FR-PROF-007's "nothing
  activates without passing" is a property of the code path, not of the
  repository's automation.
- **No archive container.** A bundle is a directory. Choosing `.zip` vs
  `.tar.zst` is a packaging decision that wants its own ticket, and the
  intake rules are identical either way.
- **No signature or authorship verification.** `provenance` is a required
  *string*; nothing checks that it is true.
