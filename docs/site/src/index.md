# RetroForge

RetroForge is an NES and SNES emulator that runs your own games exactly as
the console did, and can then add to them: no sprite flicker, widescreen,
whole-level maps, 3D, and live game info such as lives and health.

**Playing?** Start with [Getting started](getting-started.md), or look
around first in the [screenshot tour](tour.md).

## Building with RetroForge

ROADMAP P9's exit criterion is the reason the second half of this site exists:

> a third party ships a profile + pack + script without touching Rust or
> asking us questions.

So it covers exactly the three things that takes, and nothing else. The
architecture, the requirements register and the design deep-dives stay in
the repository for people working *on* RetroForge; this is for people
building *with* it.

| You want to… | Chapter |
|---|---|
| teach RetroForge about a game — addresses, decode rules, HUD, text | [Profiles](profiles.md) |
| replace a game's graphics, or ship a bundle others install | [Replacement packs](packs.md) |
| write a script or a sandboxed plugin | [Plugin SDK](plugins.md) |

## Why nothing here is copy-pasted

Every code sample on this site is `{{#include}}`d from a file the build
already compiles, validates or runs. There is **one copy** of each
sample, in the repository, and the site quotes it.

That is deliberate and it is the ticket's third criterion. Documentation
that restates code drifts from it silently — this project has been bitten
by exactly that, by `EMULATION_CORES.md` §3.3's superseded
`dropped_by_limit` line — and a doc site is the worst place for it,
because it is the copy a stranger trusts. A CI step
(`.github/scripts/verify-doc-samples.mjs`) refuses any chapter that
carries an inline `rust`, `toml` or `lua` block instead of an include, so
the rule is enforced rather than remembered.

**What that guarantees:** a sample shown here compiles or validates,
because it is the same bytes CI ran.
**What it does not:** that the surrounding prose is accurate. Prose is
still prose.
