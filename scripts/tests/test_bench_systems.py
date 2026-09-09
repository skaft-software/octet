"""Stdlib-only, synthetic regressions for the systems benchmark contract.

No product, provider, shell command, or network service is launched.
"""

from __future__ import annotations

import argparse
from contextlib import contextmanager, ExitStack, redirect_stdout, redirect_stderr
import importlib.util
import io
import json
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest
from unittest import mock


SCRIPT = Path(__file__).resolve().parents[1] / "bench-systems.py"
SPEC = importlib.util.spec_from_file_location("bench_systems", SCRIPT)
assert SPEC is not None and SPEC.loader is not None
bench = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(bench)


class Clock:
    def __init__(self) -> None:
        self.now = 1_000_000.0

    def monotonic(self) -> float:
        return self.now

    def sleep(self, seconds: float) -> None:
        if seconds < 0:
            raise AssertionError("negative sleep")
        self.now += seconds


class Process:
    def __init__(self, clock: Clock, pid: int, exit_at: float | None = None, code: int = 0):
        self.pid = pid
        self.clock = clock
        self.exit_at = exit_at
        self.code = code

    def poll(self) -> int | None:
        return self.code if self.exit_at is not None and self.clock.now >= self.exit_at else None


def memory(rss=1024, pss=None, cpu=0.0):
    return {"rss_bytes": rss, "pss_bytes": pss, "cpu_percent": cpu}


@contextmanager
def resource_fixture(*, memory_reader=None, launch=None):
    clock = Clock()
    processes = []

    def popen(argv, **kwargs):
        if launch is not None:
            process = launch(clock, len(processes), argv, kwargs)
        else:
            process = Process(clock, len(processes) + 100)
        processes.append(process)
        return process

    with ExitStack() as stack:
        stack.enter_context(mock.patch.object(bench.time, "monotonic", clock.monotonic))
        stack.enter_context(mock.patch.object(bench.time, "sleep", clock.sleep))
        # Catch any regression to a mixed clock/unit calculation.
        stack.enter_context(mock.patch.object(bench.time, "perf_counter_ns", return_value=9_000_000_000_000))
        stack.enter_context(mock.patch.object(bench.subprocess, "Popen", side_effect=popen))
        reads = stack.enter_context(mock.patch.object(
            bench, "read_memory", side_effect=memory_reader or (lambda pid: memory())
        ))
        cleanup = stack.enter_context(mock.patch.object(bench, "terminate_process"))
        yield clock, processes, reads, cleanup


class StatisticsTests(unittest.TestCase):
    def test_linear_p95_and_unavailable_are_not_zero(self):
        self.assertEqual(bench.summarize([]), {
            "count": 0, "min": None, "median": None, "p95": None, "max": None,
        })
        self.assertEqual(bench.summarize([0])["median"], 0)
        self.assertEqual(bench.summarize([0])["count"], 1)
        self.assertEqual(bench.quantile([1, 9, 5], 0.95), 8.6)
        self.assertEqual(bench.summarize([1, 9, 5])["median"], 5)

    def test_resource_summaries_use_one_peak_per_completed_run(self):
        runs = [
            {"status": "completed", "launch_ms": 1, "rss_peak_kib": 10,
             "pss_peak_kib": None, "cpu_peak_percent": 0},
            {"status": "completed", "launch_ms": 2, "rss_peak_kib": 30,
             "pss_peak_kib": None, "cpu_peak_percent": 2},
            {"status": "timeout", "launch_ms": 3, "rss_peak_kib": 1000,
             "pss_peak_kib": None, "cpu_peak_percent": 100},
        ]
        summary = bench.resource_summary(runs)
        self.assertEqual(summary["rss_peak_kib"]["median"], 20)
        self.assertEqual(summary["rss_peak_kib"]["p95"], 29)
        self.assertEqual(summary["rss_peak_kib"]["count"], 2)
        self.assertEqual(summary["pss_peak_kib"]["count"], 0)
        self.assertEqual(summary["completed_runs"], 2)
        self.assertEqual(summary["failed_runs"], 1)
        self.assertEqual(summary["launch_ms"]["count"], 3)


class ArgumentTests(unittest.TestCase):
    def test_commands_are_argument_vectors_not_shells(self):
        name, argv = bench.parse_named_command(" case =program 'two words' ';' '$HOME' '*.json'")
        self.assertEqual(name, "case")
        self.assertEqual(argv, ["program", "two words", ";", "$HOME", "*.json"])
        self.assertNotIn("shell", bench.launch_kwargs())
        self.assertEqual(bench.idle_launch_kwargs()["stdin"], subprocess.PIPE)
        self.assertEqual(bench.launch_kwargs()["stdout"], subprocess.DEVNULL)

    def test_malformed_commands_are_argparse_errors(self):
        for command in ("bad", "=program", "x=", "x='' arg", 'x="unterminated', "x=program a\0b"):
            with self.subTest(command=command), self.assertRaises(argparse.ArgumentTypeError):
                bench.parse_named_command(command)

    def test_all_invalid_arguments_are_rejected_before_launch(self):
        invalid = [
            ["--repetitions=0"], ["--timeout-seconds=0"], ["--timeout-seconds=-1"],
            ["--idle-seconds=0"], ["--idle-seconds=-1"], ["--settle-seconds=-1"],
            ["--binary="], ["--binary=bad\0exe"], ["--command=x='"],
            *[[f"--{option}-seconds={value}"]
              for option in ("timeout", "idle", "settle") for value in ("nan", "inf", "-inf")],
            *[[f"--concurrency={value}"] for value in ("", "0", "-1", "1,,2", "1,no", "1,")],
        ]
        for args in invalid:
            with self.subTest(args=args), mock.patch.object(sys, "argv", [str(SCRIPT), *args]), \
                    mock.patch.object(bench.subprocess, "Popen") as launch, redirect_stderr(io.StringIO()):
                with self.assertRaises(SystemExit) as error:
                    bench.main()
                self.assertEqual(error.exception.code, 2)
                launch.assert_not_called()

    def test_cli_names_version_case_and_passes_timeout_to_concurrency(self):
        with tempfile.TemporaryDirectory() as directory:
            output = Path(directory) / "result.json"
            argv = [str(SCRIPT), "--binary=fixture", "--idle-command=idle=fixture",
                    "--skip-idle", "--concurrency=1,2", "--timeout-seconds=7",
                    "--repetitions=2", "--output", str(output)]
            with mock.patch.object(sys, "argv", argv), \
                    mock.patch.object(bench, "benchmark_startup", return_value={}) as startup, \
                    mock.patch.object(bench, "benchmark_concurrency", return_value={}) as concurrent, \
                    mock.patch.object(bench, "benchmark_idle") as idle, \
                    mock.patch.object(bench, "print_summary"), redirect_stdout(io.StringIO()):
                self.assertEqual(bench.main(), 0)
            startup.assert_called_once_with("version_command", ["fixture", "--version"], 2, 7)
            concurrent.assert_called_once_with("idle", ["fixture"], [1, 2], 2, 1.0, 0.25, 7)
            idle.assert_not_called()
            report = json.loads(output.read_text())
            self.assertEqual(report["schema"], "octet.systems-benchmark.v2")
            self.assertIn("does not revise", report["methodology"]["history"])
            self.assertIn("No cold-cache", report["methodology"]["startup"])

    def test_skip_flags_do_not_reduce_retained_repetitions(self):
        argv = [str(SCRIPT), "--skip-startup", "--skip-concurrency",
                "--idle-command=idle=fixture", "--repetitions=9"]
        with mock.patch.object(sys, "argv", argv), \
                mock.patch.object(bench, "benchmark_startup") as startup, \
                mock.patch.object(bench, "benchmark_concurrency") as concurrency, \
                mock.patch.object(bench, "benchmark_idle", return_value={}) as idle, \
                mock.patch.object(bench, "print_summary"):
            bench.main()
        startup.assert_not_called()
        concurrency.assert_not_called()
        self.assertEqual(idle.call_args.args[2], 9)


class ResourceTests(unittest.TestCase):
    def test_idle_uses_seconds_deadline_and_retains_missing_rss_samples(self):
        with resource_fixture(memory_reader=lambda pid: memory(None, 2048, 0.0)) as (_, processes, _, cleanup):
            report = bench.benchmark_idle("idle", ["fixture"], 2, 0.12, 0.2, 1)
        self.assertEqual(report["completed_runs"], 2)
        self.assertEqual(report["failed_runs"], 0)
        self.assertEqual(report["sample_count"], 6)
        self.assertEqual(report["rss_kib"]["count"], 0)
        self.assertEqual(report["pss_kib"]["count"], 6)
        self.assertEqual(report["pss_peak_kib"]["count"], 2)
        self.assertEqual(report["cpu_peak_percent"]["median"], 0)
        self.assertNotIn("startup_ms", report)
        for run in report["runs"]:
            self.assertEqual(run["sample_count"], 3)
            self.assertAlmostEqual(run["samples"][0]["elapsed_ms"], 200, places=4)
            self.assertAlmostEqual(run["observation_ms"], 320, places=4)
            self.assertEqual(run["status"], "completed")
        self.assertEqual(cleanup.call_count, len(processes))

    def test_timeout_includes_settle_and_bounds_observation(self):
        with resource_fixture() as (clock, _, reads, cleanup):
            run = bench.measure_resources(["fixture"], 1, 1, 10, 0.2)
        self.assertEqual(run["status"], "timeout")
        self.assertEqual(run["errors"], ["measurement_timeout"])
        self.assertAlmostEqual(run["observation_ms"], 200, places=4)
        self.assertEqual(run["sample_count"], 0)
        reads.assert_not_called()
        cleanup.assert_called_once()
        with resource_fixture():
            report = bench.benchmark_idle("idle", ["fixture"], 1, 1, 0, 0.12)
        self.assertEqual(report["failed_runs"], 1)
        self.assertGreater(len(report["runs"][0]["samples"]), 0)
        self.assertIsNotNone(report["runs"][0]["rss_peak_kib"])
        self.assertEqual(report["rss_peak_kib"]["count"], 0)

    def test_launch_overrun_and_probe_overrun_are_timeouts(self):
        def launch(clock, index, argv, kwargs):
            clock.sleep(0.3)
            return Process(clock, 100 + index)

        with resource_fixture(launch=launch) as (_, processes, reads, cleanup):
            run = bench.measure_resources(["fixture"], 2, 1, 0, 0.2)
        self.assertEqual(run["status"], "timeout")
        self.assertEqual(len(processes), 1)
        reads.assert_not_called()
        cleanup.assert_called_once()
        with resource_fixture() as (clock, _, reads, _):
            def slow_probe(pid):
                clock.sleep(0.3)
                return memory()
            reads.side_effect = slow_probe
            run = bench.measure_resources(["fixture"], 1, 0.1, 0, 0.2)
        self.assertEqual(run["status"], "timeout")
        self.assertEqual(run["sample_count"], 1)
        self.assertAlmostEqual(run["samples"][0]["sample_finished_ms"], 300, places=4)

    def test_early_zero_or_nonzero_exit_fails_both_resource_modes(self):
        for code in (0, 7):
            def launch(clock, index, argv, kwargs):
                return Process(clock, index + 100, clock.now + 0.01, code)
            with self.subTest(code=code), resource_fixture(launch=launch):
                report = bench.benchmark_idle("idle", ["fixture"], 1, 1, 0.2, 3)
                concurrent = bench.benchmark_concurrency("idle", ["fixture"], [2], 1, 1, 0.2, 3)
            self.assertEqual(report["exit_codes"], [code])
            self.assertEqual(report["runs"][0]["status"], "early_exit")
            level = concurrent["levels"][0]
            self.assertEqual(level["failed_runs"], 1)
            self.assertEqual(level["runs"][0]["exit_codes_before_cleanup"], [code, code])
            self.assertEqual(level["rss_peak_kib"]["count"], 0)

    def test_failed_launch_retains_trial_and_cleans_partial_concurrency(self):
        def launch(clock, index, argv, kwargs):
            if index == 1:
                raise FileNotFoundError("fixture")
            return Process(clock, 100)
        with resource_fixture(launch=launch) as (_, processes, _, cleanup):
            run = bench.measure_resources(["fixture"], 2, 1, 0, 3)
        self.assertEqual(run["status"], "launch_error")
        self.assertEqual(run["errors"], ["FileNotFoundError"])
        self.assertEqual(run["sample_count"], 0)
        self.assertIsNone(run["rss_peak_kib"])
        cleanup.assert_called_once_with(processes[0])
        with resource_fixture(launch=lambda *args: (_ for _ in ()).throw(FileNotFoundError())):
            report = bench.benchmark_idle("idle", ["fixture"], 2, 1, 0, 3)
        self.assertEqual(len(report["runs"]), 2)
        self.assertEqual(report["failed_runs"], 2)
        self.assertEqual(report["exit_codes"], [None, None])

    def test_concurrency_complete_totals_and_raw_per_pid_samples(self):
        def read(pid):
            return memory(1024 if pid == 100 else None, 2048, 0)
        with resource_fixture(memory_reader=read):
            report = bench.benchmark_concurrency("idle", ["fixture"], [2], 1, 0.1, 0, 1)
        level = report["levels"][0]
        self.assertEqual(level["completed_runs"], 1)
        self.assertIsNone(level["rss_peak_kib"]["median"])
        self.assertEqual(level["pss_peak_kib"]["median"], 4)
        self.assertEqual(level["cpu_peak_percent"]["median"], 0)
        sample = level["runs"][0]["samples"][0]
        self.assertIsNone(sample["rss_total_kib"])
        self.assertEqual(sample["pss_total_kib"], 4)
        self.assertEqual([p["pid"] for p in sample["processes"]], [100, 101])
        self.assertEqual(report["resource_scope"], "direct_processes_only")

    def test_interrupted_probe_still_cleans_every_process(self):
        def interrupted(pid):
            raise KeyboardInterrupt
        with resource_fixture(memory_reader=interrupted) as (_, _, _, cleanup):
            with self.assertRaises(KeyboardInterrupt):
                bench.measure_resources(["fixture"], 2, 1, 0, 3)
        self.assertEqual(cleanup.call_count, 2)


class OSProbeTests(unittest.TestCase):
    def test_linux_pss_is_unavailable_without_smaps_not_rss_substitute(self):
        with mock.patch.object(bench.sys, "platform", "linux"), \
                mock.patch.object(bench.Path, "read_text", side_effect=["VmRSS: 8 kB\n", PermissionError()]), \
                mock.patch.object(bench, "read_cpu_percent", return_value=0):
            sample = bench.read_memory(123)
        self.assertEqual(sample, memory(8192, None, 0))

    def test_macos_ps_reports_real_zero_and_no_pss(self):
        with mock.patch.object(bench.sys, "platform", "darwin"), \
                mock.patch.object(bench.subprocess, "run", return_value=mock.Mock(stdout="8 0.0\n")) as probe:
            sample = bench.read_memory(123)
        self.assertEqual(sample, memory(8192, None, 0))
        self.assertEqual(probe.call_args.args[0], ["ps", "-o", "rss=,pcpu=", "-p", "123"])

    def test_failed_ps_is_unavailable_not_zero(self):
        with mock.patch.object(bench.sys, "platform", "darwin"), \
                mock.patch.object(bench.subprocess, "run", side_effect=FileNotFoundError()):
            self.assertEqual(bench.read_memory(123), memory(None, None, None))
            self.assertIsNone(bench.read_cpu_percent(123))


class StartupAndCleanupTests(unittest.TestCase):
    def test_startup_retains_failures_and_excludes_cleanup_time(self):
        clock = Clock()
        success, nonzero, timeout = mock.Mock(), mock.Mock(), mock.Mock()
        def wait(code, elapsed):
            def complete(**kwargs):
                clock.sleep(elapsed)
                return code
            return complete
        success.wait.side_effect = wait(0, 0.02)
        nonzero.wait.side_effect = wait(7, 0.03)
        timeout.wait.side_effect = subprocess.TimeoutExpired(["fixture"], 1)
        with mock.patch.object(bench.time, "monotonic", clock.monotonic), \
                mock.patch.object(bench.subprocess, "Popen", side_effect=[success, nonzero, FileNotFoundError(), timeout]), \
                mock.patch.object(bench, "terminate_process", side_effect=lambda p: clock.sleep(1)) as cleanup:
            report = bench.benchmark_startup("version_command", ["fixture", "--version"], 4, 1)
        self.assertEqual(report["phase"], "command_completion")
        self.assertEqual(report["successful_runs"], 1)
        self.assertEqual(report["failed_runs"], 3)
        self.assertEqual(report["return_codes"], [0, 7, None, None])
        self.assertEqual(report["errors"], ["FileNotFoundError", "TimeoutExpired"])
        self.assertEqual(len(report["runs"]), 4)
        self.assertAlmostEqual(report["duration_ms"]["median"], 20, places=4)
        self.assertEqual(cleanup.call_count, 3)

    def test_exited_leader_still_closes_stdin_and_cleans_owned_group(self):
        process = mock.Mock(pid=123)
        process.poll.return_value = 0
        process.wait.return_value = 0
        with mock.patch.object(bench.os, "name", "posix"), \
                mock.patch.object(bench.os, "killpg", create=True) as killpg:
            bench.terminate_process(process)
        process.stdin.close.assert_called_once()
        self.assertEqual(killpg.call_args_list, [
            mock.call(123, bench.signal.SIGTERM), mock.call(123, bench.signal.SIGKILL),
        ])


class TelemetryTests(unittest.TestCase):
    def summarize_records(self, records):
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "trace.jsonl"
            path.write_text("\n".join(json.dumps({"schema": "octet.telemetry.v1", **record}) for record in records))
            return bench.telemetry_summary([str(path)])

    def test_separate_text_and_reasoning_metrics_never_synthesized(self):
        report = self.summarize_records([
            {"record": "model_request_finished", "elapsed_ms": 100, "ttft_ms": 10,
             "first_text_delta_ms": 30, "first_reasoning_delta_ms": 10},
            {"record": "model_request_finished", "elapsed_ms": 200, "ttft_ms": 0},
            {"record": "provider_retry", "ttft_ms": 999},
        ])
        self.assertEqual(report["ttft_ms"]["median"], 5)
        self.assertEqual(report["ttft_ms"]["count"], 2)
        self.assertEqual(report["first_text_delta_ms"]["median"], 30)
        self.assertEqual(report["first_text_delta_ms"]["count"], 1)
        self.assertEqual(report["first_reasoning_delta_ms"]["median"], 10)
        self.assertIsNone(report["request_samples"][1]["first_text_delta_ms"])
        self.assertIn("not first visible text", report["timing_semantics"]["ttft_ms"])
        self.assertIn("No usage aggregation", report["usage_semantics"])

    def test_invalid_and_missing_numbers_stay_unavailable(self):
        for value in (None, True, False, "1", float("nan"), float("inf"), -1, 10 ** 1000):
            with self.subTest(value=value):
                self.assertIsNone(bench.telemetry_number(value))
        report = self.summarize_records([
            {"record": "model_request_finished", "elapsed_ms": float("nan"), "ttft_ms": True},
            {"record": "tool_started", "repeated_recently": "malformed"},
            {"record": "tool_finished"},
        ])
        self.assertEqual(report["ttft_ms"]["count"], 0)
        self.assertIsNone(report["ttft_ms"]["median"])
        self.assertIsNone(report["repeated_tool_calls"])
        self.assertEqual(report["tool_elapsed_ms"]["count"], 0)
        self.assertEqual(report["first_text_delta_ms"]["count"], 0)
        json.dumps(report, allow_nan=False)
        zeros = self.summarize_records([{"record": "tool_started", "repeated_recently": 0}])
        self.assertEqual(zeros["repeated_tool_calls"], 0)

    def test_input_overlap_and_read_failures_are_explicit(self):
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "trace.jsonl"
            path.write_text(json.dumps({"schema": "octet.telemetry.v1", "record": "model_request_finished", "ttft_ms": 4}) + "\nnot json\n{}\n")
            report = bench.telemetry_summary([str(path), str(Path(directory) / "*.jsonl"), str(Path(directory) / "missing.jsonl")])
        self.assertEqual(report["model_requests"], 1)
        self.assertEqual(report["ignored_lines"], 2)
        self.assertEqual(len(report["files"]), 1)
        self.assertEqual(len(report["read_errors"]), 1)
        self.assertEqual(report["request_samples"][0]["line"], 1)

    def test_console_reports_unavailable_and_failures(self):
        with resource_fixture(launch=lambda *args: (_ for _ in ()).throw(FileNotFoundError())):
            measurement = bench.benchmark_idle("idle", ["fixture"], 1, 1, 0, 3)
        output = io.StringIO()
        with redirect_stdout(output):
            bench.print_summary({"schema": bench.SCHEMA, "environment": {"platform": "fixture"}, "measurements": [measurement]})
        self.assertIn("unavailable (n=0)", output.getvalue())
        self.assertIn("failed 1/1", output.getvalue())
        self.assertNotIn("None", output.getvalue())


if __name__ == "__main__":
    unittest.main()
