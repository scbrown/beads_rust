"""Build and measure both revisions on this runner, then enforce the noise band."""

import json
import os
import re
import subprocess
import uuid
from pathlib import Path

from calibrate_benchmarks import read_means

# Three same-binary passes in Actions run 34308045723 measured 1.1769340491.
THRESHOLD = 1.19


def compare(base: dict[str, float], candidate: dict[str, float]) -> dict:
    """Compare the benchmarks both revisions have, and name the ones only one has.

    Requiring identical inventories (aegis-pjcpza) red a required gate for any PR
    that adds or removes a benchmark, with `NO COMPARISON MADE` and no remedy. We
    compare the intersection instead, but the fail-closed guarantee is unchanged:
    an empty intersection still raises, so a vacuous comparison can never report
    success. A benchmark present in base and absent in candidate is its own
    finding — a deletion can hide a regression — so it is reported, not dropped.
    """
    shared = base.keys() & candidate.keys()
    if not shared:
        raise ValueError(
            "NO COMPARISON MADE: the base and candidate inventories share no benchmark"
        )
    ratios = {name: candidate[name] / base[name] for name in sorted(shared)}
    return {
        "benchmarks_compared": len(ratios),
        "threshold": THRESHOLD,
        "ratios": ratios,
        "added": sorted(candidate.keys() - base.keys()),
        "removed": sorted(base.keys() - candidate.keys()),
        "regressions": {
            name: ratio for name, ratio in ratios.items() if ratio > THRESHOLD
        },
    }


def main() -> int:
    root = Path.cwd()
    output = root / "target/benchmark-comparison" / uuid.uuid4().hex
    output.mkdir(parents=True, exist_ok=False)
    base = os.environ["BENCH_BASE"]
    if not re.fullmatch(r"[0-9a-f]{40}", base) or base == "0" * 40:
        raise ValueError("NO COMPARISON MADE: an exact nonzero base commit is required")
    candidate = subprocess.check_output(["git", "rev-parse", "HEAD"], text=True).strip()
    base_tree = output / "base-source"
    subprocess.run(
        ["git", "worktree", "add", "--detach", str(base_tree), base], check=True
    )
    boot_file = Path("/proc/sys/kernel/random/boot_id")
    boot = boot_file.read_text().strip()
    env = dict(os.environ, BENCH_SAMPLE_SIZE="10")
    toolchain = subprocess.check_output(
        ["rustup", "show", "active-toolchain"], text=True
    ).split()[0]
    env["RUSTUP_TOOLCHAIN"] = toolchain
    unexpected = sorted(
        k
        for k in env
        if k.startswith("BENCH_") and k not in {"BENCH_BASE", "BENCH_SAMPLE_SIZE"}
    )
    if unexpected:
        raise ValueError(f"Unexpected benchmark overrides: {unexpected}")
    env["CARGO_TARGET_DIR"] = str(root / "target")
    metadata = {
        "base": base,
        "candidate": candidate,
        "boot_id": boot,
        "toolchain": toolchain,
        "sample_size": 10,
        "passes": [],
    }
    means = []
    for name, tree in (("base", base_tree), ("candidate", root)):
        if boot_file.read_text().strip() != boot:
            raise ValueError("Runner changed between revisions")
        criterion = output / name
        env["CRITERION_HOME"] = str(criterion)
        with (output / f"{name}.log").open("w") as log:
            result = subprocess.run(
                [
                    "cargo",
                    "bench",
                    "--locked",
                    "--bench",
                    "storage_perf",
                    "--",
                    "--save-baseline",
                    "paired",
                ],
                cwd=tree,
                env=env,
                stdout=log,
                stderr=subprocess.STDOUT,
                check=False,
            )
        metadata["passes"].append({"revision": name, "exit_code": result.returncode})
        (output / "metadata.json").write_text(json.dumps(metadata, indent=2) + "\n")
        result.check_returncode()
        if boot_file.read_text().strip() != boot:
            raise ValueError("Runner changed during measurement")
        means.append(read_means(criterion, "paired"))
    report = compare(*means)
    (output / "report.json").write_text(json.dumps(report, indent=2) + "\n")
    failures = report["regressions"]
    message = f"Compared {report['benchmarks_compared']} benchmarks on one runner; {len(failures)} over {THRESHOLD}x. {failures}"
    if report["added"]:
        message += f" Added, so not compared: {report['added']}."
    if report["removed"]:
        message += (
            " Removed, so not compared — a deleted benchmark can hide a regression: "
            f"{report['removed']}."
        )
    inventory_changed = bool(report["added"] or report["removed"])
    level = "error" if failures else "warning" if inventory_changed else "notice"
    print(f"::{level}::{message}")
    if summary := os.environ.get("GITHUB_STEP_SUMMARY"):
        with Path(summary).open("a") as stream:
            stream.write(f"## Benchmark regression gate\n\n{message}\n")
    return 1 if failures else 0


if __name__ == "__main__":
    raise SystemExit(main())
