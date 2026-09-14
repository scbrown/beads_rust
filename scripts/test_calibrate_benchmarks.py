"""Calibration must measure the worst pair and refuse incomplete measurements."""

import io
import json
import os
import subprocess
import tempfile
import unittest
from pathlib import Path
from unittest.mock import patch

from calibrate_benchmarks import main, noise_band, read_means
from compare_benchmarks import compare


class CalibrationTests(unittest.TestCase):
    def test_gate_accepts_measured_noise_and_rejects_slowdown(self):
        self.assertEqual(compare({"a": 100}, {"a": 117.693405})["regressions"], {})
        self.assertEqual(compare({"a": 100}, {"a": 119})["regressions"], {})
        self.assertIn("a", compare({"a": 100}, {"a": 120})["regressions"])

    def test_gate_refuses_missing_inventory(self):
        for base, candidate in (
            ({}, {}),
            ({"a": 100.0}, {}),
            ({}, {"a": 100.0}),
            ({"a": 100.0}, {"b": 100.0}),
        ):
            with self.subTest(base=base, candidate=candidate):
                with self.assertRaises(ValueError) as caught:
                    compare(base, candidate)
                self.assertIn("NO COMPARISON MADE", str(caught.exception))

    def test_added_benchmark_compares_the_intersection_and_names_the_addition(self):
        report = compare({"a": 100.0}, {"a": 100.0, "b": 100.0})
        self.assertEqual(report["benchmarks_compared"], 1)
        self.assertEqual(report["ratios"], {"a": 1.0})
        self.assertEqual(report["added"], ["b"])
        self.assertEqual(report["removed"], [])
        self.assertEqual(report["regressions"], {})

    def test_removed_benchmark_compares_the_intersection_and_names_the_removal(self):
        report = compare({"a": 100.0, "b": 100.0}, {"a": 100.0})
        self.assertEqual(report["benchmarks_compared"], 1)
        self.assertEqual(report["ratios"], {"a": 1.0})
        self.assertEqual(report["added"], [])
        self.assertEqual(report["removed"], ["b"])
        self.assertEqual(report["regressions"], {})

    def test_inventory_change_never_masks_a_regression_in_the_shared_set(self):
        report = compare({"a": 100.0, "gone": 100.0}, {"a": 120.0, "new": 100.0})
        self.assertIn("a", report["regressions"])
        self.assertEqual(report["added"], ["new"])
        self.assertEqual(report["removed"], ["gone"])
        self.assertEqual(report["benchmarks_compared"], 1)

    def test_intersection_boundary_is_the_same_measured_noise_band(self):
        extra = {"only-in-candidate": 1.0}
        self.assertEqual(
            compare({"a": 100}, {"a": 117.693405, **extra})["regressions"], {}
        )
        self.assertEqual(compare({"a": 100}, {"a": 119, **extra})["regressions"], {})
        self.assertIn("a", compare({"a": 100}, {"a": 120, **extra})["regressions"])

    def test_paired_gate_runs_both_commits_and_exits_on_regression(self):
        from compare_benchmarks import main as paired_main

        for fail_base, ratio in ((False, 1.17), (False, 1.20), (True, 1.0)):
            with tempfile.TemporaryDirectory() as directory:
                root = Path(directory)
                calls = []

                def run(
                    command, calls=calls, fail_base=fail_base, ratio=ratio, **kwargs
                ):
                    calls.append(command)
                    if command[0] == "git":
                        return subprocess.CompletedProcess(command, 0)
                    self.assertEqual(command[-2:], ["--save-baseline", "paired"])
                    criterion = Path(kwargs["env"]["CRITERION_HOME"])
                    if fail_base:
                        return subprocess.CompletedProcess(command, 9)
                    path = criterion / "test" / "paired" / "estimates.json"
                    path.parent.mkdir(parents=True)
                    value = 100 if criterion.name == "base" else 100 * ratio
                    path.write_text(json.dumps({"mean": {"point_estimate": value}}))
                    return subprocess.CompletedProcess(command, 0)

                previous = Path.cwd()
                try:
                    os.chdir(root)
                    with (
                        patch.dict(os.environ, {"BENCH_BASE": "a" * 40}, clear=True),
                        patch("compare_benchmarks.subprocess.run", side_effect=run),
                        patch(
                            "compare_benchmarks.subprocess.check_output",
                            return_value="b" * 40,
                        ),
                    ):
                        if fail_base:
                            with self.assertRaises(subprocess.CalledProcessError):
                                paired_main()
                            self.assertEqual(len(calls), 2)
                            self.assertEqual(
                                list(
                                    (root / "target/benchmark-comparison").glob(
                                        "*/report.json"
                                    )
                                ),
                                [],
                            )
                        else:
                            self.assertEqual(paired_main(), int(ratio > 1.19))
                            self.assertEqual(len(calls), 3)
                finally:
                    os.chdir(previous)

    def test_inventory_change_is_reported_in_the_annotation_and_step_summary(self):
        """An added benchmark must land the gate green AND name itself (aegis-pjcpza).

        The report keys are covered above; this drives main() so the operator-facing
        half — the ::warning:: annotation and the step summary a PR author actually
        reads — is proven rather than assumed.
        """
        from compare_benchmarks import main as paired_main

        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            summary = root / "step-summary.md"

            def run(command, **kwargs):
                if command[0] == "git":
                    return subprocess.CompletedProcess(command, 0)
                criterion = Path(kwargs["env"]["CRITERION_HOME"])
                names = ["shared"] if criterion.name == "base" else ["shared", "fresh"]
                for name in names:
                    path = criterion / name / "paired" / "estimates.json"
                    path.parent.mkdir(parents=True)
                    path.write_text(json.dumps({"mean": {"point_estimate": 100}}))
                return subprocess.CompletedProcess(command, 0)

            previous = Path.cwd()
            try:
                os.chdir(root)
                with (
                    patch.dict(
                        os.environ,
                        {
                            "BENCH_BASE": "a" * 40,
                            "GITHUB_STEP_SUMMARY": str(summary),
                        },
                        clear=True,
                    ),
                    patch("compare_benchmarks.subprocess.run", side_effect=run),
                    patch(
                        "compare_benchmarks.subprocess.check_output",
                        return_value="b" * 40,
                    ),
                    patch("sys.stdout", new_callable=io.StringIO) as stream,
                ):
                    self.assertEqual(paired_main(), 0)
                    annotation = stream.getvalue()
            finally:
                os.chdir(previous)

            # Green, but loud: an inventory change is a warning, never a silent pass.
            self.assertIn("::warning::", annotation)
            self.assertNotIn("::error::", annotation)
            self.assertIn("fresh", annotation)
            self.assertIn("Compared 1 benchmarks", annotation)
            self.assertIn("fresh", summary.read_text())

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
