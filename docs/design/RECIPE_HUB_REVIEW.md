# Recipe hub: reviewer checklist (W40-01)

For the maintainer reviewing a submission under the moderation rule in
`docs/design/RECIPE_HUB.md`. These commands are documented, not run by the
ticket. Run ROM-gated tests with the maintainer's own copy
(`RF_SCREENS_ROMS_DIR`, default /Users/bmatthews/Games/Roms); never commit
ROMs, patches or movies. Use `CARGO_INCREMENTAL=0` and
`CARGO_TARGET_DIR=<worktree>/target`; never `--workspace`.

## 1. Intake

1. Put the submission outside the repo or on a branch under
   `profiles/<console>/<game>/` as `recipe.submission.toml`.
2. `python3 scripts/recipe-hub/validate-submission.py <file>`
   - REJECT (exit 1): stop, reply with the printed reasons.
   - HOLD (exit 2): the file has `[[patch]]` rows; ask for the facts split
     into their own file, and referee the patch rows in step 3.
   - ACCEPT / ACCEPT with clamps (exit 0): continue.
3. Read every `source` line. Reject anything that cites a map, wiki,
   disassembly or another project.

## 2. Reproduce facts (candidate to observed)

Reproduce a sample of rows on your own copy with the existing tooling:
function rows by building the atlas and matching the recipe
(`cargo test --release -p retroforge --test w36_10_rom_hacks -- --ignored --nocapture`
shows the matched/unmatched report for the SMW recipe; point a copy of that
harness at the submitted file), RAM rows by watching the address in the
debugger. Only rows you reproduced move to `observed` (edit the grade in your
own commit).

## 3. Lockstep referee

Fact merge rules (unit tests, no ROM):

    cargo test -p rf-rebuild facts
    cargo test -p rf-plugin-sdk

Hooks and patch behaviour with the submitted rows loaded, original mode must
stay byte-identical (the tests assert lockstep zero-divergence and that a
read-only or inert hook does not diverge):

    cargo test --release -p retroforge --test w36_05_hooks -- --ignored --nocapture
    cargo test --release -p retroforge --test w36_06_mod_load -- --ignored --nocapture
    cargo test --release -p retroforge --test w36_03_rebuild_smb -- --ignored --nocapture
    cargo test --release -p retroforge --test w36_10_rom_hacks -- --ignored --nocapture

Shadow-core cost and status on real cartridges:

    cargo test --release -p retroforge --test w37_04_shadow_cost -- --ignored --nocapture

For a patch row: use a replay you chose (not the submitter's), run original
mode with the patch row present and absent; any frame or state difference in
original mode rejects the row. Only then recommit it yourself as `confirmed`.

## 4. Merge

1. Rows the submitter supplied enter as `candidate` unless promoted in step 2
   or 3. The merge rule for plugins is `rf_rebuild::merge_facts`; trusted rows
   are never demoted.
2. Commit with a conventional message; record the promoting evidence
   (test name, your reproduction) in the commit body, never ROM data.
3. Update docs/STATUS.md and plan.json as usual (lead).

## 5. After merge

Disputes and takedowns follow rules 7 and 8 of the moderation rule.
