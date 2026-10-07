# Fonts bundled with RetroForge (ticket W15-07)

- **IBM Plex Sans** (Regular — body text; SemiBold — display/headings/badges),
  Copyright © 2017 IBM Corp. with Reserved Font Name "Plex". Licensed under
  the SIL Open Font License, Version 1.1 (`IBMPlexSans-OFL.txt` in this
  directory). Source: the typeface's own published distribution; no network
  fetch was performed to obtain these files (project law 2 / NFR-011).
- **IBM Plex Sans Condensed** (SemiBold — overlay titles and the library
  hero, ticket W21-01), Copyright © 2019 IBM Corp., same SIL Open Font
  License 1.1 (the upstream `LICENSE.txt` differs from
  `IBMPlexSans-OFL.txt` only in line wrapping). Fetched 2026-10-07 from
  IBM's own repository,
  `https://raw.githubusercontent.com/IBM/plex/master/packages/plex-sans-condensed/fonts/complete/ttf/IBMPlexSansCondensed-SemiBold.ttf`
  (version 3.000), sha256
  `acc2b1e3b9597ee13b08b767f845bd038fe120cbee806859af7c01b60fe6e9e6`.

All three files are embedded via `include_bytes!` in `crates/retroforge/src/theme.rs`
and registered with `egui::FontDefinitions` at startup. No other font is
shipped by this crate beyond egui's own bundled defaults (Hack, the emoji
sets) and the system's `FontFamily::Monospace`, both left untouched by this
ticket.
