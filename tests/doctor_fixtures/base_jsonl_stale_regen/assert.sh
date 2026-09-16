#!/usr/bin/env bash
# Fixture assertions: base_jsonl_stale_regen
#
# An older, different ancestor is normal common history.
# Neither diagnosis nor repair may replace it with local state.

set -euo pipefail
target_dir="${1:?usage: assert.sh <target_dir> <stage>}"
stage="${2:?usage: assert.sh <target_dir> <stage>}"
tool_bin="${TOOL_BIN:-br}"
cd "$target_dir"

case "$stage" in
  detect)
    out=$("$tool_bin" doctor --json 2>/dev/null) || true
    echo "$out" | jq -e '
      .checks[] | select(.name == "base_jsonl")
      | select(.status == "ok")
    ' >/dev/null || {
      echo "ASSERT FAIL[$stage]: valid older ancestor was not accepted" >&2
      echo "$out" | jq '.checks[] | select(.name == "base_jsonl")' >&2
      exit 1
    }

    # Stale anchor must still be on disk (regular file, not symlink),
    # and its content must match the planted stale placeholder.
    [ -f .beads/beads.base.jsonl ] || {
      echo "ASSERT FAIL[$stage]: anchor missing after detect" >&2
      exit 1
    }
    [ -L .beads/beads.base.jsonl ] && {
      echo "ASSERT FAIL[$stage]: anchor became a symlink during detect" >&2
      exit 1
    }
    if ! cmp -s .beads/beads.base.jsonl .fixture_baseline_stale; then
      echo "ASSERT FAIL[$stage]: planted stale anchor content drifted during detect" >&2
      exit 1
    fi
    ;;

  post_repair)
    # Anchor still exists at its canonical path.
    [ -f .beads/beads.base.jsonl ] || {
      echo "ASSERT FAIL[$stage]: anchor missing after --repair" >&2
      exit 1
    }
    if ! cmp -s .beads/beads.base.jsonl .fixture_baseline_stale; then
      echo "ASSERT FAIL[$stage]: repair destroyed the prior ancestor" >&2
      exit 1
    fi

    # The retired rewrite must never appear in the repair journal.
    found=""
    for run_dir in $(find .doctor/runs -mindepth 1 -maxdepth 1 -type d 2>/dev/null | sort); do
      if [ -f "$run_dir/actions.jsonl" ] && \
         grep '"fixer_id":"doctor.base_jsonl_regen"' "$run_dir/actions.jsonl" \
           | grep -q '"op":"write_file"'; then
        found="$run_dir/actions.jsonl"
        break
      fi
    done
    if [ -n "$found" ]; then
      echo "ASSERT FAIL[$stage]: unsafe ancestor rewrite recorded" >&2
      for run_dir in $(find .doctor/runs -mindepth 1 -maxdepth 1 -type d 2>/dev/null | sort); do
        echo "  --- $run_dir/actions.jsonl ---" >&2
        [ -f "$run_dir/actions.jsonl" ] && sed 's/^/    /' "$run_dir/actions.jsonl" >&2
      done
      exit 1
    fi

    # An older ancestor remains valid after unrelated repairs.
    out=$("$tool_bin" doctor --json 2>/dev/null) || true
    if echo "$out" | jq -e '
      .checks[] | select(.name == "base_jsonl")
      | select(.status == "warn")
      | select(.details.kind == "stale")
    ' >/dev/null; then
      echo "ASSERT FAIL[$stage]: stale finding still fires post-repair" >&2
      exit 1
    fi
    ;;

  post_undo)
    # Undo of unrelated repairs must also preserve common history.
    [ -f .beads/beads.base.jsonl ] || {
      echo "ASSERT FAIL[$stage]: anchor missing after undo" >&2
      exit 1
    }
    if ! cmp -s .beads/beads.base.jsonl .fixture_baseline_stale; then
      echo "ASSERT FAIL[$stage]: undo did not restore planted stale anchor bytes" >&2
      diff .beads/beads.base.jsonl .fixture_baseline_stale | head -10 >&2 || true
      exit 1
    fi
    ;;

  *)
    echo "unknown stage: $stage" >&2
    exit 2
    ;;
esac
