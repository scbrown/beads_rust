"""Calibration must measure the worst pair and refuse incomplete measurements."""

import json
import os
import subprocess
import tempfile
import unittest
from pathlib import Path
from unittest.mock import patch

from calibrate_benchmarks import main, noise_band, read_means


class CalibrationTests(unittest.TestCase):
    def test_worst_pair_is_not_only_first_versus_last(self):
        result = noise_band(
            [{"a": 100, "b": 200}, {"a": 127, "b": 150}, {"a": 110, "b": 180}]
        )
        self.assertEqual(result["worst_benchmark"], "b")
        self.assertAlmostEqual(result["maximum_ratio"], 200 / 150)
        self.assertGreater(result["proposed_threshold_ratio"], 200 / 150)
        self.assertEqual(result["benchmarks_compared"], 2)

    def test_identical_is_a_real_zero_swing(self):
        result = noise_band([{"a": 100}] * 3)
        self.assertEqual(result["maximum_swing_percent"], 0)
        self.assertGreater(result["proposed_threshold_ratio"], 1)

    def test_missing_or_mismatched_pass_is_not_a_clean_comparison(self):
        cases: list[list[dict[str, float]]] = [
            [],
            [{}] * 3,
            [{"a": 1}] * 2,
            [{"a": 1}, {"a": 1}, {"b": 1}],
        ]
        for passes in cases:
            with self.subTest(passes=passes), self.assertRaises(ValueError):
                noise_band(passes)

    def test_invalid_estimates_are_not_zero_noise(self):
        for value in (0, -1, float("nan"), float("inf")):
            with self.subTest(value=value), self.assertRaises(ValueError):
                noise_band([{"a": value}] * 3)

    def test_read_only_exact_fresh_baseline(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            for baseline, value in (("old", 999), ("fresh", 123)):
                path = root / "group" / "size" / baseline / "estimates.json"
                path.parent.mkdir(parents=True)
                path.write_text(json.dumps({"mean": {"point_estimate": value}}))
            self.assertEqual(read_means(root, "fresh"), {"group/size": 123})
            with self.assertRaises(ValueError):
                read_means(root, "absent")

    def test_malformed_estimate_is_loud(self):
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "group" / "fresh" / "estimates.json"
            path.parent.mkdir(parents=True)
            for value in (True, "100", None, 0, float("nan")):
                path.write_text(json.dumps({"mean": {"point_estimate": value}}))
                with (
                    self.subTest(value=value),
                    self.assertRaises((ValueError, TypeError)),
                ):
                    read_means(Path(directory), "fresh")

    def run_fixture(self, fail_second=False):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            binary = root / "fixture"
            binary.write_text("unchanging fixture binary")
            calls = []

            def run(command, **kwargs):
                calls.append(command)
                if command[0] == "cargo":
                    return subprocess.CompletedProcess(
                        command,
                        0,
                        json.dumps(
                            {
                                "reason": "compiler-artifact",
                                "target": {"name": "storage_perf"},
                                "executable": str(binary),
                            }
                        ),
                    )
                self.assertEqual(
                    command[:3], [str(binary), "--bench", "--save-baseline"]
                )
                self.assertEqual(kwargs["env"]["BENCH_SAMPLE_SIZE"], "10")
                if fail_second and len(calls) == 3:
                    return subprocess.CompletedProcess(command, 9)
                path = Path(kwargs["env"]["CRITERION_HOME"]) / "nested" / command[3]
                path.mkdir(parents=True)
                (path / "estimates.json").write_text(
                    json.dumps({"mean": {"point_estimate": 100 + len(calls)}})
                )
                return subprocess.CompletedProcess(command, 0)

            previous = Path.cwd()
            try:
                os.chdir(root)
                with (
                    patch.dict(os.environ, {"BENCH_SAMPLE_SIZE": "10"}, clear=True),
                    patch("calibrate_benchmarks.subprocess.run", side_effect=run),
                    patch(
                        "calibrate_benchmarks.subprocess.check_output",
                        return_value="fixture",
                    ),
                ):
                    if fail_second:
                        with self.assertRaises(subprocess.CalledProcessError):
                            main()
                        self.assertFalse(
                            (root / "target/benchmark-calibration/report.json").exists()
                        )
                        self.assertEqual(len(calls), 3)
                    else:
                        main()
                        report = json.loads(
                            (
                                root / "target/benchmark-calibration/report.json"
                            ).read_text()
                        )
                        self.assertEqual(report["benchmarks_compared"], 1)
                        self.assertEqual(len(calls), 4)
                        self.assertEqual(len({call[3] for call in calls[1:]}), 3)
            finally:
                os.chdir(previous)

    def test_orchestration_runs_one_build_and_three_unique_passes(self):
        self.run_fixture()

    def test_failed_second_pass_never_writes_success_report(self):
        self.run_fixture(fail_second=True)


if __name__ == "__main__":
    unittest.main()
