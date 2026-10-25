# W36-00: rebuild research sources (2026-10-25)

Summary of the research behind `docs/design/REBUILD_LAB.md`. Read: READMEs,
design docs and public write-ups only. No emulator, recompiler or
decompilation source code was read for this summary. Licence tiers per D-025.

## Recompilers and ports (licence → what we may read)

| Project | Licence | Reading tier |
|---|---|---|
| [gb-recompiled](https://github.com/arcanite24/gb-recompiled) | MIT | Source readable for ideas (D-025), nothing copied |
| [nes-to-sms](https://github.com/akitaonrails/nes-to-sms) | MIT OR Apache-2.0 | Source readable for ideas (D-025), nothing copied |
| [snesrecomp](https://github.com/mstan/snesrecomp) ([multi-tier design](https://raw.githubusercontent.com/mstan/snesrecomp/HEAD/docs/MULTI_TIER.md)) | PolyForm Noncommercial | Docs only |
| [nesrecomp](https://github.com/mstan/nesrecomp), [SuperMarioBrosNESRecomp](https://github.com/mstan/SuperMarioBrosNESRecomp), [SuperMarioWorldRecomp](https://github.com/mstan/SuperMarioWorldRecomp) | PolyForm Noncommercial | Docs only |
| [DKC1Recomp](https://raw.githubusercontent.com/elliotttate/DKC1Recomp/HEAD/README.md) | PolyForm Noncommercial; metadata with GPL-3 provenance | README only |
| [zelda3](https://github.com/snesrev/zelda3) | decompilation | Excluded (D-021); cited only as the "user supplies the ROM, assets extracted locally" distribution model |
| [Zelda64Recomp](https://github.com/Zelda64Recomp/Zelda64Recomp), [RT64](https://github.com/rt64/rt64) | GPL-3 | Excluded (D-018); cited for the distribution model only |

## Ideas taken (in our words)

- Ship recipes and our own code, rebuild from the player's ROM locally
  (zelda3 / N64Recomp distribution model).
- Multi-tier execution: recompiled code where proven, interpreter fallback
  for anything unproven or self-modifying (snesrecomp design doc).
- Widescreen gameplay by overriding only object culling, activation and OAM
  packing (DKC1Recomp README).
- Binary translation needs runtime-observed targets for indirect jumps:
  [DEC FX!32](https://web.stanford.edu/class/cs343/resources/fx32.pdf),
  [Elevator static translation (2026)](https://arxiv.org/pdf/2605.08419).

## Context

- [SNES](https://recompsoftware.com/platforms/snes/) and
  [NES](https://recompsoftware.com/platforms/nes/) recomp/decomp lists.
- [snesrecomp's first title: Super Mario World](https://1379.tech/snesrecomps-first-title-super-mario-world/) (2026-05-21);
  [five months of recomp with AI](https://1379.tech/nes-snes-genesis-virtualboy-and-psx-a-journey-with-ai-and-recompilation/).
- Community reaction to AI ports:
  [decomp.dev closing](https://www.videogameschronicle.com/news/one-of-the-biggest-game-decompilation-sites-is-closing-because-ai-has-broken-the-community-aspect/),
  [recomp devs vs AI ports](https://kotaku.com/zelda-and-banjo-recompiled-devs-go-to-war-with-ai-vibe-coded-slopcomps-2000739936).
