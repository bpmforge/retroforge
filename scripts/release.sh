#!/usr/bin/env bash
# Release engineering v0 (ticket W5-05).
#
# Usage: scripts/release.sh <version>        e.g. scripts/release.sh v0.1.0
#
# Prepares a release and records the evidence for it. It deliberately does
# NOT tag or push: tagging is a human decision, and a script that tags is
# a script that can tag the wrong thing.
#
# WHAT IT PRODUCES, and which criterion each part answers:
#
#   1. Host-platform artifacts, and an honest list of the platforms it
#      cannot produce (criterion 1, as amended 2026-08-23).
#   2. The release's golden .rfstate/.rfreplay fixtures, archived into
#      fixtures/releases/<version>/ (criterion 2, FR-STATE-005).
#   3. Release notes carrying rf-harness's accuracy table (criterion 3).
#   4. An EXECUTED migration drill over every previously archived
#      release, with its result recorded (criterion 4, R-F3).
#
# ON CRITERION 1, stated rather than hidden: this builds for the host
# platform only. The macOS/Windows/Linux matrix needs machines this
# project does not have — GitHub Actions is out of budget permanently
# (ruling 2026-08-22) and cross-building Windows and Linux from darwin is
# not a substitute, because wgpu makes it awkward and an artifact nobody
# has RUN on the target OS is not evidence of a release. A self-hosted
# runner or Gitea Actions on `origin` would restore the matrix; both are
# free and neither is set up.
set -euo pipefail

script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
repo_root="$(cd "$script_dir/.." && pwd)"
cd "$repo_root"

version="${1:-}"
if [ -z "$version" ]; then
  echo "usage: scripts/release.sh <version>   e.g. scripts/release.sh v0.1.0" >&2
  exit 2
fi

# A release describes a commit. Building one from a dirty tree produces
# evidence that names a commit it did not actually test -- the same
# failure validate-evidence rejects for the local gate.
if [ -n "$(git status --porcelain)" ]; then
  echo "release: working tree is dirty; commit or stash first" >&2
  echo "  a release must describe a commit, not a commit plus edits" >&2
  exit 1
fi
commit="$(git rev-parse HEAD)"

out_dir="$repo_root/docs/releases/$version"
fixture_dir="$repo_root/fixtures/releases/$version"
artifact_dir="$repo_root/target/release-artifacts/$version"
mkdir -p "$out_dir" "$fixture_dir" "$artifact_dir"

# ---------------------------------------------------------------------------
# 4 FIRST: the migration drill runs BEFORE this release's fixtures are
# archived, or it would "verify" the fixtures it just wrote and prove
# nothing. Order is the whole point of R-F3.
# ---------------------------------------------------------------------------
echo "release: migration drill over previously archived releases..." >&2
drill_log="$out_dir/migration-drill.txt"
drill_status="pass"
# BOTH halves: R-F3 names ".rfstate/.rfreplay". The state drill lives in
# rf-state and the replay drill in rf-input, because parsing a replay is
# rf-input's job and reaching across for it would put a cross-crate
# dependency in a test purely to avoid creating a file.
{
  echo "--- .rfstate drill (rf-state) ---"
  cargo test --release -p rf-state --test golden_fixture -- --nocapture 2>&1 || echo "STATE_DRILL_FAILED"
  echo
  echo "--- .rfreplay drill (rf-input) ---"
  cargo test --release -p rf-input --test release_replay_drill -- --nocapture 2>&1 || echo "REPLAY_DRILL_FAILED"
} > "$drill_log"
if grep -q "STATE_DRILL_FAILED\|REPLAY_DRILL_FAILED" "$drill_log"; then
  drill_status="FAIL"
fi
prior_count="$(find "$repo_root/fixtures/releases" \( -name '*.rfstate' -o -name '*.rfreplay' \) 2>/dev/null | wc -l | tr -d ' ')"
echo "release: drill $drill_status over $prior_count previously archived fixture(s)" >&2
if [ "$drill_status" = "FAIL" ]; then
  echo "release: a previously released fixture no longer loads. See $drill_log" >&2
  echo "  That is a migration this project owes its users (FR-STATE-005)." >&2
  exit 1
fi

# ---------------------------------------------------------------------------
# 2. Archive THIS release's golden fixtures.
# ---------------------------------------------------------------------------
echo "release: archiving golden fixtures..." >&2
archived=0
for f in \
  "$repo_root/crates/rf-state/tests/fixtures/golden_v1.rfstate" \
  "$repo_root/crates/retroforge/tests/fixtures/nrom-frame6.rfstate" \
  "$repo_root/fixtures/replays/unprofiled-scroller.rfreplay"; do
  if [ -f "$f" ]; then
    cp "$f" "$fixture_dir/"
    archived=$((archived + 1))
  else
    echo "release: WARNING fixture missing, not archived: $f" >&2
  fi
done
echo "release: archived $archived fixture(s) to fixtures/releases/$version/" >&2

# ---------------------------------------------------------------------------
# 1. Host artifacts.
# ---------------------------------------------------------------------------
host="$(rustc -vV | awk '/^host:/{print $2}')"
echo "release: building host artifacts for $host..." >&2
cargo build --release -p retroforge -p retroforge-tool
for bin in retroforge retroforge-tool; do
  if [ -f "$repo_root/target/release/$bin" ]; then
    cp "$repo_root/target/release/$bin" "$artifact_dir/$bin-$host"
  fi
done

# ---------------------------------------------------------------------------
# 3. Release notes, carrying the accuracy table.
# ---------------------------------------------------------------------------
echo "release: writing notes..." >&2
node "$script_dir/release-notes.mjs" \
  "$version" "$commit" "$host" "$prior_count" "$drill_status" "$archived" \
  > "$out_dir/RELEASE_NOTES.md"

echo "release: OK" >&2
echo "  notes      $out_dir/RELEASE_NOTES.md" >&2
echo "  fixtures   fixtures/releases/$version/ ($archived)" >&2
echo "  artifacts  $artifact_dir ($host only)" >&2
echo "  drill      $drill_status over $prior_count prior fixture(s)" >&2
echo >&2
echo "  NOT DONE BY THIS SCRIPT: tagging and publishing. Tag when you mean to." >&2
