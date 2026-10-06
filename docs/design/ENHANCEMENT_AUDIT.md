# Enhancement promise audit (2026-10-06)

**Question:** of every enhancement this project has built and closed a
ticket for, which can a player actually *reach* from the running app —
and does what the app shows match what it does?

**Method.** Re-verified on branch `ui/wave-17` (from `main` `e80a883`):

- **verified (test)** — an end-to-end test drives the real
  `RetroForgeApp` and asserts the effect; I re-ran it on 2026-10-06 and
  it passed (command below).
- **verified (code)** — I traced the path from a UI control to the code
  that does the work, by reading it; no test drives it end to end.
- **verified absent** — I searched `crates/retroforge/src` and found no
  caller; the grep is quoted.
- **inferred** — believed from code reading, not executed.
- **by eye: Brad** — only a human looking at a real title can judge it.

Re-run on 2026-10-06, all green (11 binaries, 14 tests):

```
cargo test -p retroforge --test deflicker_reaches_the_app --test diorama_reaches_the_app \
  --test full_level_reaches_the_app --test hdpack_reaches_the_app --test widescreen_reaches_the_app \
  --test map_tab_is_fed --test diorama_live_view --test mode7_ground_live_view \
  --test sprite_overlay_mode_invariant --test upscale_studio_ui --test enhance_workspace
```

Photographs: `target/ui-tour-before/` holds the shipped tour
(`capture_tour.rs`) taken before any Wave 20 change; `target/ui-tour/`
holds the tour after.

## 1. Reachable, and does what it says

| Feature | Ticket | Route in the app | Evidence |
|---|---|---|---|
| Sprite-limit bypass | W3-03 | Enhance › Features row (Enhanced mode) → `CoreCommand::SetSpriteOverlay` | verified (test): `sprite_overlay_mode_invariant` (state hashes identical on/off over 20 frames; off by default) |
| De-flicker (temporal) | W3-05 | Enhance › Features row → `SetDeflicker`; restored on reopen (app.rs ~2700) | verified (test): `deflicker_reaches_the_app` (pixels change) |
| Generic ultrawide camera | W4-03e | View › Camera: Ultrawide | verified (code): app.rs:4092; `map_tab_is_fed` exercises the same stitcher feed |
| Map tab (stitched canvas) | W10-02 | Enhance workspace › Map | verified (test): `map_tab_is_fed` |
| Full-level view | W5-03/W11-02 | Enhance › Features (Game-Aware + profile with `[decode]`) | verified (test): `full_level_reaches_the_app` on RF-Scroller. **Only 2 of the 11 shipped profiles have a `[decode]` table** (`nes/rf-scroller`, `nes/rf-scroller-demo`) — see §3 |
| Decoded widescreen (SNES) | W11-03 | Enhance › Features → `SetWidescreen` | verified (test): `widescreen_reaches_the_app`. Not inert — the review's claim to that effect is **refuted**; but see §3 for the reopen gap |
| Diorama walls | W16-13 | Enhance / Game Settings (Game-Aware + collision profile) | verified (test): `diorama_live_view`, `diorama_reaches_the_app` (live texture changes, badge names it). by eye on a real title: Brad |
| Mode 7 ground | W16-14 | Game Settings › "Diorama: Mode 7" when BG mode 7 is live | verified (test): `mode7_ground_live_view` on a synthetic mode-7 frame. by eye on Super Mario Kart / Pilotwings: Brad |
| Upscale Studio (offline) | W16-02 | Enhance menu › Upscale Studio… (app.rs:4173) | verified (test): `upscale_studio_ui` (stub upscaler; writes only approved tiles) |

## 2. Built, closed, and NOT reachable from the app

Each was checked with `grep -rn "<symbol>" crates/retroforge/src`
excluding comments.

| Feature | Ticket | What exists | Gap | Evidence |
|---|---|---|---|---|
| **Mesen HD packs** | W11-05 | `RetroForgeApp::load_hd_pack`/`clear_hd_pack` (app.rs:7632/7686) and the compositor path at app.rs:2893 | **No menu item, button or setting calls `load_hd_pack`** — only `tests/hdpack_reaches_the_app.rs` does. The ticket's first acceptance line ("a user can import a Mesen HD pack from the app") is not met by the UI. The review listed this as reachable; that is **refuted** | verified absent: `load_hd_pack` has no caller in `src/` |
| **Fog / steam pass** | W16-04 | `rf_renderer::fog::FogPass` (golden-tested), app-side converters `enhanced_view::atmosphere_density_rgba` / `atmosphere_scroll_drift_per_second` | **`FogPass` is never constructed by the app**; the converters are called only from their own unit tests. Yet the "Atmosphere: fog" row reads ON and counts toward the badge once the Game Settings ladder is set to Active. **The badge would claim an enhancement nothing draws** — the one thing principle 2 forbids | verified absent: `FogPass` appears only in `rf-renderer/tests/fog_golden.rs` and `rf-renderer/src/bin/bench_passes` |
| **MetalFX spatial** | W16-08 | Settings › Video radio, badge suffix (`enhance_ui::append_metalfx_badge_suffix`) | **`MetalFxScaler` is never constructed by the app.** In a build with `--features metalfx` on Apple Silicon the radio enables and the badge gains a MetalFX suffix, but no frame is scaled by it. In the default build the radio is disabled, so the lie only appears in a metalfx build | verified absent (code): no `MetalFxScaler`/`metalfx::` call in `src/`; runtime behaviour inferred (no metalfx build run here) |
| Shader chain | W3-02/W3-02a | `rf_renderer::ShaderChain` + 6 shaders + param descriptors | Settings shows a free-text "Shader" box read by nothing | verified absent |
| Rewind | W8-02 | `rf_state::rewind::RewindRing` | No reference in `src/`; no hotkey | verified absent |
| Video recording | W8-03 | `crate::recording::Recorder` (APNG) | Module compiled into the crate but never constructed by `app.rs` | verified absent |
| Loading fast-forward | W8-07 | `rf_enhance::loading::FastForward` | No reference in `src/`. Two shipped profiles declare `[[loading.wait_loops]]` (`rf-scroller`, `rf-scroller-demo`) | verified absent |
| HUD separation | W8-06 | `rf_enhance::hud::HudSeparator` | No reference in `src/` | verified absent |
| Translation / accessibility overlays | W8-12 | `rf_enhance::overlay::Overlays` | No reference; also no glyph rasteriser for replacement text | verified absent |
| Smooth camera / room stitching | W8-08 | `rf_enhance::experiments` | No reference | verified absent |
| Frame interpolation | W8-09 | `rf_enhance::interpolation` (candidacy only) | Blending declined by design (FRAME_INTERPOLATION.md §7); nothing to wire | verified absent, by design |

## 3. Reachable, but the UI says more than the machine does

| # | Gap | Evidence |
|---|---|---|
| A | **Full-level view on a profile without `[decode]`.** The row's availability is `profile_gated_features_unlocked(profile_matched)` (enhance_ui.rs:99/136) — any matched profile. `LevelSession::open` returns `None` without `[decode]` (level_view.rs:60-68), and `set_level_probe` then sends `SetLevelProbe(None)`. So on Metroid, Zelda, SMB, and every SNES profile the toggle turns on, counts toward the badge, and does nothing | verified (code); a failing test is the first step of W20-09 |
| B | **Fog row** (above): ON + badge-counted with no render | verified (code) |
| C | **MetalFX suffix** (above): badge suffix with no scaler | verified (code), runtime inferred |
| D | **Decoded widescreen is not re-applied on reopen.** `open_rom_path` restores sprite overlay, de-flicker and full-level view from the per-game settings but not `widescreen_decoded`, so a game saved with it on reopens showing the toggle ON and a 4:3 picture. And `Stepper::set_widescreen` only acts on the SNES core (stepper.rs:1028), so the row is meaningful only on SNES | verified (code) |
| E | **SNES `WidescreenPolicies`** are set from the profile's `[widescreen]` table inside `set_widescreen` (app.rs:8122-8127) — user-facing only through that one toggle; no per-layer control is exposed (by design: the profile decides policy, the user decides on/off) | verified (code) |

### Status after W20-09 (branch `ui/wave-17`)

| Row | Fixed by W20-09 | Proof |
|---|---|---|
| A — full-level without `[decode]` | Row is `NeedsGameState("a level map in this game's profile")` unless a level decoded; never badge-counted otherwise | `enhance_ui` unit test `full_level_view_needs_a_decoded_level_not_just_a_profile` |
| B — fog row | Row is unavailable ("the fog renderer, which the live view does not run yet") until W20-17 wires `FogPass`; never ON, never counted | `fog_is_not_counted_while_nothing_renders_it` |
| C — MetalFX suffix | Suffix requires a scaler that actually ran (`METALFX_SCALER_WIRED = false`); the Settings radio is disabled with "Not used by the play view yet" | `metalfx_suffix_absent_when_no_scaler_ran` |
| D — widescreen reopen / NES | Re-applied on ROM open when saved on and effective; row unavailable on NES | `widescreen_reaches_the_app` reopen check (fails with the fix disabled — checked); `widescreen_is_snes_only` |
| §2 HD packs | File › Load HD pack… / Remove HD pack (NES only), import summary as a toast; a pack is dropped when another game opens; the badge gains "· HD pack" and the breakdown its summary | `hdpack_reaches_the_app` (badge names it; reopen drops it). The menu route itself needs a native folder dialog, so it is **verified (code)**, not driven by a test |

## 4. Not built

- **W16-07** real-time AI pass — `todo`, `hold: true` (Brad, "stay with
  MetalFX"; measured 335 ms/frame vs the 16.67 ms gate).

## 5. What I could not confirm

- Anything by eye on a commercial title (no ROMs in this worktree; law
  5). Diorama and Mode 7 on real games remain **by eye: Brad**, as their
  tickets already say.
- MetalFX at runtime (needs a `--features metalfx` build on Apple Silicon
  and a human looking at it).
- Fog on a real fog scene (needs the pass wired first, then A Link to
  the Past's Lost Woods by eye).

## 6. What Wave 20 does about it

- **W20-09** (Phase 1, honesty): full-level row gated on a decoded level;
  widescreen re-applied on reopen and SNES-only; fog row not counted as
  on while nothing renders it; MetalFX suffix only when a scaler runs;
  a "Load HD pack…" route so W11-05's first acceptance line is true.
- **W20-02**: the shader chain.
- **W20-13 / W20-14**: rewind and recording.
- **W20-17** (Phase 3): loading fast-forward, HUD separation, and the
  fog pass in the live view if it fits the budget gate; overlays, smooth
  camera and interpolation stay unwired with the reasons above.
