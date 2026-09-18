# Game profiles live here: profiles/<console>/<game-slug>/profile.toml
See docs/design/GAME_PROFILES.md.

## Index (ticket W11-06: ten profiles, breadth across families and consoles)

| Title | Console | Decoder family | Identity verified against | Sources |
|---|---|---|---|---|
| RF-Scroller (fixture) | NES | metatile_screens | in-tree fixture build | fixtures/nes/rf-scroller |
| RF-Scroller-Demo (fixture) | NES | metatile_screens | in-tree fixture build | fixtures/nes/rf-scroller |
| RF-Rooms (fixture) | NES | room_grid | in-tree fixture build | fixtures/nes/rf-rooms |
| The Legend of Zelda | NES | room_grid (shape only, no room_grid table) | not verified -- no identity, profile-only | datacrystal.tcrf.net/wiki/The_Legend_of_Zelda/RAM_map |
| Super Mario Bros. | NES | none | sha256 5dde3850...9a62db | datacrystal.tcrf.net/wiki/Super_Mario_Bros./RAM_map |
| Metroid | NES | none | sha256 649db803...03dec7bb | datacrystal.tcrf.net/wiki/Metroid/RAM_map |
| RF-Rooms-Flat (fixture) | SNES | room_grid | in-tree fixture build | fixtures/snes/rf-rooms-flat |
| RF-Scroller-S (fixture) | SNES | none (widescreen only) | sha256 fd77844d...c059de7a | fixtures/snes/rf-scroller-s |
| example-mode7 (fixture) | SNES | custom (Mode 7 plugin) | in-tree fixture build | fixtures/snes -- see profile |
| Super Mario World | SNES | none | sha256 0838e531...7137c27f5b | sites.google.com/site/smwrammap (SMW Central RAM map) |
| Super Metroid | SNES | none | sha256 12b77c4b...798cc2ca72 | jathys.zophar.net/supermetroid/kejardon/RAMMap.txt |

Ten profiles ship; six are new commercial-title or documented-layout
profiles added across W11-06/W16-05, four are this project's own fixtures.
Both consoles are represented, and two decoder families (`metatile_screens`,
`room_grid`) plus the `custom`/undeclared cases give the count breadth
rather than ten profiles of one shape (the acceptance criterion this table
exists to make checkable at a glance). Full identity hashes and the
`title_probe`-verified `[[memory_map]]` rows are in each profile's own
header comment (D-009: local-only verification against a developer-held
ROM counts as evidence once the profile records the normalized hash it
was checked against).
