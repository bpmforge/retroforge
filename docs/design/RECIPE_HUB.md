# Recipe hub (ticket W40-01)

Status: draft, awaiting Brad's approval. The hub does not open, and no
external submission is accepted, until Brad approves the moderation rule
below (REBUILD_LAB.md section 9: "a community recipe hub is approved once a
moderation rule exists").

A recipe is a data file of facts about a game (docs/design/GAME_PROFILES.md,
"Rebuild recipes"). The hub is how people outside the project submit and
review such files. It is a review queue in front of the repo, not a store of
game content: nothing the hub holds can reproduce, patch or describe a
game's code.

## Moderation rule

Status: draft, awaiting Brad's approval.

**1. What a submission may contain.** One `recipe.toml` in the existing
recipe format (`format = 1`) and nothing else. Allowed: offsets and
addresses, RAM and function names, shape/exact hashes, counts, strides,
grades, and a `source` note on every row saying how the fact was observed.
Allowed sources start with `observed:`, `rule:`, `finder:` or `ai:`.

**2. What it may never contain.** ROM bytes of any kind (hex dumps, byte
arrays, base64), patches or IPS/BPS data, disassembly or decompilation (or
text derived from either), movies or save states, and anything whose source
is a community map, wiki, forum post, disassembly project or another
person's decompilation (D-021, D-024, law 5). A row whose only source is
"it is documented somewhere" is rejected: the fact must be one that
RetroForge tooling can observe on the submitter's own copy of the game.
Submitters state in the pull request that every row was observed on their
own copy and that they hold the right to submit it.

**3. Intake check.** `tools/recipe-hub/validate-submission.py` runs first.
Any REJECT verdict ends review. It checks schema (the exact keys the loader
accepts), banned provenance words, byte payloads, hash and range shapes, and
grades. It is a filter, not a verdict on honesty: a human reviewer reads
every `source` line before anything merges.

**4. Grades.** An incoming row is always treated as `candidate`, whatever
grade the file claims (the same clamp as the fact-plugin merge in
`rf-rebuild::facts`). A candidate never drives a patch and never drives a
hook on a host that can write memory. A row already `observed` or
`confirmed` in the repo is never demoted by a submission; a disagreeing
submission is recorded as a conflict and left for review, not applied.

**5. Who can promote.** Only the lead (a maintainer) promotes, by editing the
grade in a reviewed commit: `candidate` to `observed` after the maintainer
reproduces the fact with RetroForge tooling on their own copy, `observed` to
`confirmed` only after a lockstep run passes (rule 6) or a person with the
game checks it. Submitters, plugins and the hub itself cannot promote.

**6. Lockstep is the referee.** The hub accepts facts only. A `[[patch]]`
row is never merged straight from a submission (fact plugins refuse files
that carry them). A patch row is accepted only if shadow lockstep keeps the
original mode (mod off) byte-identical on the submitter-independent replay:
a replay recorded or chosen by the maintainer, not supplied by the
submitter, run against the maintainer's own image with and without the
patch. Any divergence in original mode rejects the row. A patch row that
passes is recommitted by the maintainer as `confirmed`; the submitter's text
is not used. Fact rows that disagree with lockstep behaviour (a hook that
diverges, a function that fails to match) are rejected the same way.
Procedure: `docs/design/RECIPE_HUB_REVIEW.md`.

**7. Disputes.** Anyone may open an issue against a merged row, with
observation steps. The maintainer reproduces it; a row that cannot be
reproduced is demoted to `candidate` or removed. Decisions are written in the
issue with the reason. The maintainer's decision stands; Brad is the final
appeal.

**8. Takedowns.** A row, recipe or submitter can be removed at any time by
the maintainer, with no notice, if it is found to contain or derive from
prohibited content, or on a rightsholder or submitter request. Removal is a
commit that deletes the rows; if prohibited bytes ever reached git history,
history is rewritten and the incident is recorded in DECISIONS.md. A
submitter who submits prohibited content knowingly is banned.

**9. Hub opening.** The hub (public intake instructions, the submission
template, any hosted queue) stays closed until Brad approves this rule in
writing in the project chat. Approval is recorded as a D-number in
docs/DECISIONS.md. Changes to this rule need the same approval.

## What exists now

| piece | status |
|---|---|
| This rule | draft, awaiting Brad |
| `tools/recipe-hub/validate-submission.py` | built; checked on made-up samples |
| `docs/design/RECIPE_HUB_REVIEW.md` | lead's referee checklist (commands only) |
| Real external submission accepted | pending: needs Brad's approval and a real submitter |

The in-repo samples under `tools/recipe-hub/samples/` are for a made-up
engine and are not game content; the "first external recipe accepted"
acceptance is not met by them.
