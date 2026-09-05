#!/usr/bin/env bash
# Report how close each CI job runs to its timeout-minutes, without mutating anything.
#
# WHY (aegis-44vjem). Three jobs in this repo have crossed their budget in turn,
# and each was diagnosed only after it started failing:
#
#   Test Suite       105.2m of work in a 90m budget, masked by an assertion
#                    failure that exited the drain early (split in add88c8c)
#   Benchmarks       succeeded at 27.6m on 2026-09-02, then every main run since
#                    is `cancelled` at exactly 60.2m
#   Code Coverage    `cancelled` at 30.2m against 30m — nobody had noticed
#
# A job doing N independent compiles inside one budget will cross that budget as
# the suite grows. The crossing is invisible until it happens, and then it is
# ambiguous: GITHUB REPORTS A JOB TIMEOUT AS `cancelled`, which reads as fail-fast
# collateral from a sibling failure rather than as a wall. That ambiguity is why
# Benchmarks sat untriaged for days and why Code Coverage was missed entirely.
#
# So: measure headroom BEFORE the crossing, from real run durations, and compare
# each job against the budget IN THE COMMIT THAT RAN — not the one in the working
# tree. Reading the local file is how you get a table that is confidently wrong;
# measured while writing this, a 4-week-stale checkout produced budgets that had
# nothing to do with the runs being examined.
#
# Usage:
#   scripts/audit-workflow-timeout-headroom.sh [--repo OWNER/NAME] [--runs N] [--warn PCT]
#
# Exit: 0 all jobs under the warn threshold
#       1 at least one job at or over it (or already timing out)
#       2 could not measure (no gh, no runs) — distinct from a clean report

set -euo pipefail

python3 - "$@" <<'PY'
from __future__ import annotations

import argparse
import base64
import datetime as dt
import json
import subprocess
import sys

import yaml


def gh(*args: str) -> str:
    p = subprocess.run(["gh", *args], capture_output=True, text=True)
    if p.returncode != 0:
        return ""
    return p.stdout


def main() -> int:
    ap = argparse.ArgumentParser(
        description="Report CI job duration against the timeout budget that applied to each run."
    )
    ap.add_argument("--repo", default="scbrown/beads_rust")
    ap.add_argument("--workflow", default="ci.yml")
    ap.add_argument("--branch", default="main")
    ap.add_argument("--runs", type=int, default=5)
    ap.add_argument("--warn", type=float, default=80.0,
                    help="percent of budget at which a job is reported (default 80)")
    a = ap.parse_args()

    listing = gh("run", "list", "--repo", a.repo, "--workflow", a.workflow,
                 "--branch", a.branch, "--limit", str(a.runs),
                 "--json", "databaseId,headSha,createdAt")
    if not listing.strip():
        print("CANNOT TELL: no runs returned (gh unavailable, or no runs on "
              f"{a.repo}:{a.branch})", file=sys.stderr)
        return 2
    runs = json.loads(listing)

    # job name -> list of (percent, mins, budget, conclusion, date)
    seen: dict[str, list] = {}
    measured_runs = 0

    for r in runs:
        # THE BUDGET THAT APPLIED TO THIS RUN, read at the run's own commit.
        raw = gh("api", f"repos/{a.repo}/contents/.github/workflows/{a.workflow}"
                        f"?ref={r['headSha']}", "--jq", ".content")
        if not raw.strip():
            continue
        try:
            ci = yaml.safe_load(base64.b64decode(raw))
        except Exception:
            continue
        budget: dict[str, int | None] = {}
        for jid, j in (ci.get("jobs") or {}).items():
            name = j.get("name", jid)
            t = j.get("timeout-minutes")
            budget[name] = t
            budget[name.split(" (")[0]] = t          # matrix jobs render as "Name (variant)"

        jobs_raw = gh("run", "view", str(r["databaseId"]), "--repo", a.repo, "--json", "jobs")
        if not jobs_raw.strip():
            continue
        measured_runs += 1
        for j in json.loads(jobs_raw)["jobs"]:
            if not j.get("startedAt") or not j.get("completedAt"):
                continue
            s = dt.datetime.fromisoformat(j["startedAt"].replace("Z", "+00:00"))
            e = dt.datetime.fromisoformat(j["completedAt"].replace("Z", "+00:00"))
            mins = (e - s).total_seconds() / 60
            t = budget.get(j["name"]) or budget.get(j["name"].split(" (")[0])
            if not t:
                continue
            seen.setdefault(j["name"], []).append(
                (100 * mins / t, mins, t, j.get("conclusion"), r["createdAt"][:10]))

    if not measured_runs:
        print("CANNOT TELL: no run could be measured", file=sys.stderr)
        return 2

    rows = []
    for name, obs in seen.items():
        worst = max(obs, key=lambda o: o[0])
        rows.append((name, *worst, len(obs)))
    rows.sort(key=lambda r: -r[1])

    print(f"{a.repo}:{a.branch} {a.workflow} — worst of {measured_runs} run(s), "
          f"each against its own commit's budget\n")
    print(f"{'job':40s} {'worst':>6s} {'mins':>6s} {'budget':>7s} {'concl':10s} {'when':10s}")
    flagged = 0
    for name, pct, mins, t, concl, when, n in rows:
        mark = ""
        if concl == "cancelled" and pct >= 99:
            mark = "  <- TIMING OUT (github reports a timeout as `cancelled`)"
            flagged += 1
        elif pct >= a.warn:
            mark = "  <- low headroom"
            flagged += 1
        if pct >= a.warn or mark:
            print(f"{name[:40]:40s} {pct:5.0f}% {mins:6.1f} {t:7d} {concl or '?':10s} {when}{mark}")

    if flagged == 0:
        print(f"(no job at or above {a.warn:.0f}% of its budget)")
    print()
    return 1 if flagged else 0


sys.exit(main())
PY
