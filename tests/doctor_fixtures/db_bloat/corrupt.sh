#!/usr/bin/env bash
# Fixture: db_bloat
# FM: fm-caches_indexes-db-bloat-vs-jsonl
#
# Builds a valid workspace whose JSONL is large enough to make the ratio
# meaningful, then appends zero pages to the SQLite database. SQLite accepts
# the file and VACUUM rewrites it back to the logical database size, which
# makes this a deterministic end-to-end check for the unsafe-auto-fix path
# without issuing destructive SQL in the fixture.

set -euo pipefail
target_dir="${1:?usage: corrupt.sh <target_dir>}"
tool_bin="${TOOL_BIN:-br}"

mkdir -p "$target_dir"
cd "$target_dir"
"$tool_bin" init >/dev/null 2>&1

# The detector intentionally ignores tiny workspaces. Keep this comfortably
# above DB_BLOAT_MIN_JSONL_BYTES (1 MiB) with a single valid issue record.
# Create it through the public CLI so this fixture follows the current JSONL
# schema instead of carrying a hand-written record that normalization rejects.
payload_file=".fixture_large_description"
head -c 1200000 /dev/zero | tr '\0' 'x' > "$payload_file"
"$tool_bin" create "db bloat fixture" --type task --priority 2 \
  --description-file "$payload_file" >/dev/null
"$tool_bin" sync --flush-only >/dev/null 2>&1

sqlite3 .beads/beads.db 'PRAGMA wal_checkpoint(TRUNCATE); PRAGMA integrity_check;' \
  | grep -Fxq ok

# Add 18 MiB of trailing zero pages. SQLite still reports integrity_check=ok,
# and VACUUM compacts the database back to its logical page set.
dd if=/dev/zero bs=1048576 count=18 status=none >> .beads/beads.db
sqlite3 .beads/beads.db 'PRAGMA integrity_check;' | grep -Fxq ok

wc -c < .beads/beads.db > .fixture_db_bloat_pre_bytes
wc -c < .beads/issues.jsonl > .fixture_jsonl_bytes
sha256sum .beads/beads.db | awk '{print $1}' > .fixture_db_bloat_pre_sha256

printf 'BR_DOCTOR_FIXTURE_REPAIR_ARGS=--unsafe-auto-fix --only fm-caches_indexes-db-bloat-vs-jsonl\n' \
  > .fixture_env

if [ -e .fixture_baseline ]; then
  echo "fixture baseline already exists; expected a fresh workspace" >&2
  exit 1
fi
mkdir -p .fixture_baseline
tar --exclude=.fixture_baseline -cf .fixture_baseline/state.tar .
