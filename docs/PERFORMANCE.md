# Performance Strategy (index)

One page: where every performance rule lives, plus the budgets. Details stay
in the owning docs (single source of truth); this is the map (TRACEABILITY
G10).

## Budgets (provisional until Phase 1/6 benchmarks make them binding — SRS NFR-002)

| Path | Budget | Enforced by |
|---|---|---|
| NES core frame (Accuracy) | ≤ 2 ms on one M-class core | criterion bench, >10% regression blocks merge (TESTING.md) |
| SNES core frame (Accuracy) | ≤ 8 ms | same |
| Enhancement frame-sync work (`on_frame`) | ≤ 2 ms; heavy work goes to workers | bench + runtime meter |
| Render (enhanced, ultrawide) | 60 fps sustained | soak test (PLAYBOOK tiers) |
| Audio | no underruns over 5-min soak | W2-05 acceptance |
| Debugger idle overhead | == compiled-out within bench noise | EMULATION_CORES.md pay-for-use rule |
| Plugin callbacks | per-frame budget, host-throttled | PLUGINS.md §4 |

## Where the strategy lives

- **Threading model & what may parallelize** (core thread sacred; workers
  for post-processing/decode/AI/traces): ARCHITECTURE.md §6.
- **Core hot paths** (catch-up scheduling, event-mask zero-cost, master-cycle
  accounting): docs/design/EMULATION_CORES.md.
- **GPU pipeline costs** (render-to-texture layering, shader chains,
  headless CI): docs/design/RENDERER.md.
- **Async/caching** (content-addressed cache, canvas persistence, AI jobs
  off-path): docs/design/ENHANCEMENT_RUNTIME.md §3/§6, rf-cache.
- **Bench/soak gates and tiers**: docs/TESTING.md + PLAYBOOK.md.

Rule of thumb for implementers: measure first (criterion/`--timings`), the
frame path allocates nothing per-frame after warm-up, and any new
per-frame work needs a bench row before it merges.
