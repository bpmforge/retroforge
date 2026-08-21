# Replacement packs

Two related things live here:

- a **replacement pack** swaps a game's graphics, keyed by tile identity;
- a **bundle** is how a pack, a profile or a mod is distributed to other
  people, and it is licence-gated.

## Pack formats

RetroForge reads **Mesen's `hires.txt`** so existing community packs
load, and has its own AI/artist pack format for packs it builds. They are
different formats on purpose: one is an interchange surface we did not
design and cannot change, the other is ours to evolve.

Matching is **hash-based, never fuzzy**, and the palette is part of a
tile's identity — the same tile bytes under two palettes are two
different pictures, and keying on pixels alone is what produces the
classic artefact where a recoloured enemy wears its sibling's skin.

Tile lookup is three-tier, which is Mesen's rule and not an optimisation:

1. a rule whose `(tile data, palette)` both match;
2. otherwise a `default tile` rule matching the tile data alone;
3. otherwise the original tile is drawn.

Tier 2 is what makes palette-keying safe. Without it an author must
enumerate every palette a tile ever appears under, and the ones they miss
fall through to the original — which is how a sprite ends up
half-replaced.

### A pack that cannot be fully satisfied is reported, not silently applied

If an image is missing, a rule is gated on a condition this build cannot
evaluate, or a replacement region runs off its sheet, the import is
**PARTIAL** and every gap names the specific tile. The usable rules stay
usable — refusing the whole pack would throw away work the author did do
— but a pack that half-applies *without saying so* is the worst outcome
available: some tiles change, some do not, and nobody can tell which was
intended.

## Distributing a bundle

The distribution format is specified in `docs/design/DISTRIBUTION.md`.
The part worth reading before you package anything:

{{#include ../../design/DISTRIBUTION.md:allowlist}}

## Licence metadata is required

{{#include ../../design/DISTRIBUTION.md:licence}}

Run the check yourself before submitting:

```text
rf-intake <bundle-dir>     # 0 accepted, 1 refused, 2 unreadable
```
