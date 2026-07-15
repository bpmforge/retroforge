# RetroForge — Decision ledger

Append-only. Row format follows the exemplar (repopulse FOUNDING_BRIEF):
D-id · topic (dated when added post-founding) · decision. Amendments get new
rows, never edits. Bundle-adopted decisions are flagged **vetoable**.

| ID | Topic | Decision |
|---|---|---|
| D-000 | Founding package (2026-07-06) | The Phase-0 doc set (VISION, SCOPE, NON_GOALS, CONSTRAINTS, ARCHITECTURE, design/*, SRS, TESTING, ROADMAP, plan.json) is the locked baseline. Revisiting a NON_GOAL requires a design doc, not a ticket. |
| D-001 | Self-contained fixture doctrine (added 2026-07-15, from Brad: "this should be self contained") | Game-shaped content the project demos, gates, or regresses on is **ours, in-repo, built from source in CI** with permissive/CC0 licensing: RF-Scroller (NES, cc65/neslib, W2-10) and RF-Scroller-S (SNES, libSFX, via W6-00). Nova the Squirrel 1/2 are removed from every gate, FR, demo, and profile plan (kills license slates FS-1/FS-2; RISKS R-16 re-scoped). Boundary: hardware-**accuracy oracles** (SingleStepTests, blargg, gilyon, PeterLemon, undisbeliever, nestest) stay external fetch-only — community ground truth is worth more than self-containment there. Alter Ego (PD) remains a secondary independent-proof smoke fixture. Generality honesty: fixture demos prove the *pipeline*; proof on games we didn't design comes from the unprofiled-fixture stitcher demo + user-side profiles of community-documented commercial titles (facts only, user ROMs). Design: docs/work/IMPROVEMENT_RECOMMENDATIONS.md §G. **Vetoable** (adopted from a plain-sentence direction). |
