# Fonts bundled with RetroForge (ticket W15-07)

- **IBM Plex Sans** (Regular — body text; SemiBold — display/headings/badges),
  Copyright © 2017 IBM Corp. with Reserved Font Name "Plex". Licensed under
  the SIL Open Font License, Version 1.1 (`IBMPlexSans-OFL.txt` in this
  directory). Source: the typeface's own published distribution; no network
  fetch was performed to obtain these files (project law 2 / NFR-011).

Both files are embedded via `include_bytes!` in `crates/retroforge/src/theme.rs`
and registered with `egui::FontDefinitions` at startup. No other font is
shipped by this crate beyond egui's own bundled defaults (Hack, the emoji
sets) and the system's `FontFamily::Monospace`, both left untouched by this
ticket.
