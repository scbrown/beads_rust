"""Measure three complete passes of one binary; never enable enforcement."""

import hashlib
import json
import math
import os
import subprocess
import time
import uuid
from pathlib import Path


def read_means(root: Path, baseline: str) -> dict[str, float]:
    means = {}
    for path in root.glob(f"**/{baseline}/estimates.json"):
        value = json.loads(path.read_text())["mean"]["point_estimate"]
        if isinstance(value, bool) or not isinstance(value, (int, float)):
            raise TypeError(f"Non-numeric mean: {path}")
        if not math.isfinite(value) or value <= 0:
            raise ValueError(f"Non-positive or non-finite mean: {path}")
        means[str(path.parent.parent.relative_to(root))] = value
    if not means:
        raise ValueError(f"NO COMPARISON MADE: no estimates for {baseline}")
    return means


def noise_band(passes: list[dict[str, float]]) -> dict:
    if len(passes) != 3 or not passes[0]:
        raise ValueError("Three nonempty complete passes are required")
    names = set(passes[0])
    if any(set(values) != names for values in passes):
        raise ValueError("NO COMPARISON MADE: benchmark sets differ across passes")
    ratios = {}
    for name in sorted(names):
        values = [p[name] for p in passes]
        if any(not math.isfinite(v) or v <= 0 for v in values):
            raise ValueError(f"Invalid estimate for {name}")
        ratios[name] = max(values) / min(values)
    worst = max(ratios, key=ratios.__getitem__)
    maximum = ratios[worst]
    return {
        "benchmarks_compared": len(names),
        "maximum_ratio": maximum,
        "maximum_swing_percent": (maximum - 1) * 100,
        "worst_benchmark": worst,
        "ratios": ratios,
        # Round up, then leave one further percentage point above the maximum.
        # This is a measured proposal, not an automatic policy change.
        "proposed_threshold_ratio": math.ceil(maximum * 100) / 100 + 0.01,
        "enforcement": "ADVISORY; runner pinning and review still required",
    }


def sha256(path: Path) -> str:
    with path.open("rb") as stream:
        return hashlib.file_digest(stream, "sha256").hexdigest()


def main() -> None:
    output = Path("target/benchmark-calibration")
    output.mkdir(parents=True, exist_ok=False)
    build = subprocess.run(
        [
            "cargo",
            "bench",
            "--locked",
            "--bench",
            "storage_perf",
            "--no-run",
            "--message-format=json",
        ],
        text=True,
        stdout=subprocess.PIPE,
        check=True,
    )
    (output / "build.jsonl").write_text(build.stdout)
    binaries = {
        row["executable"]
        for line in build.stdout.splitlines()
        if (row := json.loads(line)).get("reason") == "compiler-artifact"
        and row.get("target", {}).get("name") == "storage_perf"
        and row.get("executable")
    }
    if len(binaries) != 1:
        raise ValueError(f"Expected exactly one benchmark executable: {binaries}")
    binary = Path(binaries.pop()).resolve()
    binary_hash = sha256(binary)
    commit = subprocess.check_output(["git", "rev-parse", "HEAD"], text=True).strip()
    boot_id = Path("/proc/sys/kernel/random/boot_id").read_text().strip()
    metadata = {
        "commit": commit,
        "binary_sha256": binary_hash,
        "boot_id": boot_id,
        "runner_name": os.environ.get("RUNNER_NAME"),
        "run_id": os.environ.get("GITHUB_RUN_ID"),
        "run_attempt": os.environ.get("GITHUB_RUN_ATTEMPT"),
        "sample_size": os.environ.get("BENCH_SAMPLE_SIZE", "10"),
        "rustc": subprocess.check_output(["rustc", "-Vv"], text=True),
        "cpu": Path("/proc/cpuinfo").read_text(),
        "passes": [],
    }
    env = dict(
        os.environ,
        BENCH_SAMPLE_SIZE=metadata["sample_size"],
        CRITERION_HOME=str(Path("target/criterion").resolve()),
    )
    unexpected = sorted(
        key for key in env if key.startswith("BENCH_") and key != "BENCH_SAMPLE_SIZE"
    )
    if unexpected:
        raise ValueError(f"Unexpected benchmark overrides: {unexpected}")
    # Names are fresh even if an unrelated cache contains old Criterion results.
    prefix = f"calibration-{uuid.uuid4().hex}"
    passes = []
    for index in range(1, 4):
        if sha256(binary) != binary_hash:
            raise ValueError("Benchmark executable changed between passes")
        if Path("/proc/sys/kernel/random/boot_id").read_text().strip() != boot_id:
            raise ValueError("Runner changed between passes")
        baseline = f"{prefix}-{index}"
        started = time.time()
        with (output / f"pass-{index}.log").open("w") as log:
            result = subprocess.run(
                [str(binary), "--bench", "--save-baseline", baseline],
                env=env,
                stdout=log,
                stderr=subprocess.STDOUT,
                check=False,
            )
        metadata["passes"].append(
            {
                "baseline": baseline,
                "started_unix": started,
                "duration_seconds": time.time() - started,
                "exit_code": result.returncode,
            }
        )
        (output / "metadata.json").write_text(json.dumps(metadata, indent=2) + "\n")
        result.check_returncode()
        # CRITERION_HOME above makes output independent of Cargo cache settings.
        means = read_means(Path("target/criterion"), baseline)
        (output / f"pass-{index}.json").write_text(json.dumps(means, indent=2) + "\n")
        passes.append(means)
        print(f"Pass {index}: {len(means)} benchmarks completed", flush=True)
    report = noise_band(passes)
    (output / "report.json").write_text(json.dumps(report, indent=2) + "\n")
    message = (
        f"Measured {report['benchmarks_compared']} benchmarks over three passes; "
        f"maximum swing {report['maximum_swing_percent']:.6f}% "
        f"at {report['worst_benchmark']}. Proposed threshold ratio "
        f"{report['proposed_threshold_ratio']:.2f}; enforcement remains advisory."
    )
    print(f"::notice::{message}")
    if summary := os.environ.get("GITHUB_STEP_SUMMARY"):
        with Path(summary).open("a") as stream:
            stream.write(f"## Benchmark calibration\n\n{message}\n")


if __name__ == "__main__":
    main()
