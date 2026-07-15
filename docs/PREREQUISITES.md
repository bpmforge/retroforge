# RetroForge — Human prerequisites (the ONLY human steps, per phase)

Design review G-29 (2026-07-15). Rows are the human/founder actions the board
cannot automate. A missing HP row parks the dependent ticket as
`blocked` with a note naming the HP id — agents never improvise around one.

| # | When | Step | Consumed by |
|---|---|---|---|
| HP-1 | ~~before W5 demo work~~ | ~~Nova 1 licensing posture~~ **Superseded 2026-07-15 by D-001 (self-contained fixture doctrine)** — Nova removed from all gates/demos | — |
| HP-2 | ~~before W6~~ | ~~Nova 2 fixture rights~~ **Superseded 2026-07-15 by D-001** — RF-Scroller-S (in-repo libSFX fixture) replaces it via W6-00 | — |
| HP-3 | before profile authoring (W5-01) | Sign off the DataCrystal facts-only transcription policy (CONSTRAINTS §2, GFDL 1.2) — founder slate FS-3, vetoable | FR-PROF-003, GAME_PROFILES §3 |
| HP-4 | before W3-06 | Confirm GitHub Actions macOS/Windows runner budget (3-OS CI is billable beyond the free tier for private repos; public repo = free) | W3-06, W5-05, NFR-009 |
| HP-5 | before W7 exit | Supply 3 designated plain-LoROM commercial ROMs (user-owned dumps) for the P7 exit sample — never committed, local only | ROADMAP P7 exit |
| HP-6 | anytime (goodwill, unblocks vendoring) | File upstream license requests: SingleStepTests/65816 (ask for MIT to match sibling repos), PeterLemon/SNES, christopherpow/nes-test-roms | TESTING §3 no-vendor rule |
| HP-7 | before overnight build (P7 launch) | Confirm Gitea remote reachability from the build machine (origin is LAN-bound; GitHub is the always-push remote) | PLAYBOOK git rules, CLAUDE.md law 8 |

Rules: the product never automates an HP row; agents cite the HP id in block
notes; this table is append-only (superseded rows get ~~strikethrough~~ +
a pointer, never deletion).
