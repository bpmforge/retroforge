# Release checklist (ticket W5-05)

Run `scripts/release.sh <version>`. It performs steps 3–6 and refuses to
start on a dirty tree, because a release describes a **commit** — evidence
naming a commit it did not actually test is the failure
`validate-evidence` already rejects for the local gate.

The script **does not tag and does not publish.** Tagging is a decision;
a script that tags is a script that can tag the wrong thing.

| # | Step | How it is evidenced |
|---|---|---|
| 1 | Full gate green | `cargo fmt --all --check`, `clippy -D warnings`, `cargo test --workspace`, `validate-arch`, `validate-plan`, `validate-traceability`, `validate-evidence`, `cargo deny check licenses`, `verify-doc-samples`, `scripts/docs-gate.sh` |
| 2 | `scripts/local-gate.sh` green | Regenerates `docs/evidence/local-gate.json` and runs the ten artifact-backed suites the workspace gate skips |
| 3 | **Migration drill EXECUTED** (R-F3, FR-STATE-005) | `docs/releases/<version>/migration-drill.txt` — every previously archived `.rfstate` still loads |
| 4 | This release's fixtures archived | `fixtures/releases/<version>/` |
| 5 | Host artifacts built | `target/release-artifacts/<version>/` |
| 6 | Release notes with the accuracy table | `docs/releases/<version>/RELEASE_NOTES.md` |
| 7 | Tag and push — **by hand** | `git tag <version> && git push github <version> && git push origin <version>` |

## Why the drill runs before the archive

Step 3 runs **before** step 4, and the order is the entire point of R-F3.
Archive first and the drill "verifies" the fixtures it has just written,
which proves nothing while looking green. FR-STATE-005 is about *previous*
releases loading on *current* code.

At v0 the drill finds zero prior releases. That is recorded, not skipped —
**a drill that quietly does nothing is exactly how FR-STATE-005 becomes
ceremonial**, which is the failure R-F3 was written to prevent. v0
establishes the baseline; from v1 the drill does real work. A synthetic
"previous release" was deliberately not manufactured: faking the input
would make the evidence a lie.

## What this release process cannot do, and why

**Artifacts for macOS + Windows + Linux.** Criterion 1 was amended
2026-08-23 to "every platform that can actually be built and run here,
and name the ones it cannot", for two independent reasons already ruled
on:

* GitHub is code storage for a private repo, not a distribution channel
  (2026-08-19).
* The GitHub Actions budget is not being increased (2026-08-22), so the
  3-OS matrix in `.github/workflows/ci.yml` will not run again.

Cross-building Windows and Linux from the darwin dev machine is **not** a
substitute: wgpu's graphics stack makes it awkward, and an artifact nobody
has *run* on the target OS is not evidence of a release.

Restoring the matrix is a **self-hosted runner** (GitHub does not bill
self-hosted minutes on private repos) or **Gitea Actions** on the `origin`
remote away. Both are free; neither is set up, and neither should be until
distribution is a real requirement.

## FR-STATE-005 moved gates

The requirement reads "golden `.rfstate` fixtures from each release shall
load in CI forever (or fail the build)". **There is no CI.** The
obligation now lives in the ordinary workspace gate: `rf-state`'s
`every_archived_release_fixture_still_loads` is **not** `#[ignore]`d, so
`cargo test --workspace` runs it on every pass — a stronger place than
CI ever was, since that gate is the one this project actually runs. It
walks `fixtures/releases/` rather than naming files — a hardcoded list would
need editing at every release, and the edit that gets forgotten is exactly
the one that matters.
