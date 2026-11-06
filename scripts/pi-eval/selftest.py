#!/usr/bin/env python3
"""Self-test for the eval's parsing, scoring and report logic (synthetic data, no processes)."""

from __future__ import annotations

import json
import sys
import tempfile
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
import core  # noqa: E402

TRACE = """octet-startup: process.enter elapsed=0us
octet-startup: extensions.pi_mirror.prepare.begin elapsed=10000us
octet-startup: extensions.digest.files count=3
octet-startup: frame.ready elapsed=14000us
octet-startup: extensions.pi_mirror.prepare.end elapsed=29010000us
octet-startup: extensions.process.spawn:octet-pi-compat elapsed=469us
octet-startup: adapter.factory.import.0 elapsed=500000us
octet-startup: adapter.factory.registered.0 elapsed=510000us
octet-startup: adapter.factory.import.1 elapsed=900000us
octet-startup: adapter.factory.registered.1 elapsed=910000us
octet-startup: adapter.load.ready elapsed=1000000us factories=2 failures=0
octet-startup: extensions.attached elapsed=30000000us
garbage octet-startup: not a line
octet-startup: broken elapsed=xus
"""

BRIDGE = {
    "extensions": [
        "/h/.pi/agent/npm/node_modules/pkg-a/index.ts",
        "/h/.pi/agent/npm/node_modules/@s/pkg-b/src/index.ts",
        "/h/.pi/agent/npm/node_modules/pkg-c/index.ts",
        "/h/.pi/agent/pi-eval/pi-eval-sentinel.ts",
    ],
    "mirror_excluded": [{"entry": "/h/.pi/agent/npm/node_modules/pkg-c/index.ts", "reason": "typebox import"}],
    "registrations": {"commands": [{"name": "alpha"}, {"name": "/beta"}]},
}
PROBE = {
    "extensions": [
        {"path": "/x/node_modules/pkg-a/index.ts", "resolvedPath": "/x/node_modules/pkg-a/index.ts", "commands": ["alpha"]},
        {"path": "/x/node_modules/@s/pkg-b/src/index.ts", "commands": ["beta", "gamma"]},
        {"path": "/x/node_modules/pkg-c/index.ts", "commands": []},
        {"path": "/x/pi-eval/pi-eval-sentinel.ts", "commands": ["sentinel"]},
    ],
    "errors": [{"path": "/x/node_modules/pkg-c/index.ts", "error": "Tool \"read\" conflicts"}],
}
LOCKED = ["pkg-a", "@s/pkg-b", "pkg-c", "pkg-d"]


class TraceTests(unittest.TestCase):
    def test_parse_ignores_noise(self):
        events = core.parse_trace_lines(TRACE)
        self.assertEqual(len(events), 12)
        self.assertEqual(events[0]["elapsed_us"], 0)
        self.assertEqual(events[2]["count"], 3)

    def test_summary(self):
        trace = core.summarize_trace(core.parse_trace_lines(TRACE))
        self.assertAlmostEqual(trace["derived"]["prepare_s"], 29.0)
        self.assertAlmostEqual(trace["derived"]["frame_ready_s"], 0.014)
        self.assertAlmostEqual(trace["derived"]["extensions_attached_s"], 30.0)
        self.assertAlmostEqual(trace["derived"]["adapter_load_ready_s"], 1.0)
        self.assertEqual(trace["factory_registered"], [0, 1])
        self.assertEqual(trace["load_ready"]["factories"], 2)
        self.assertIn("extensions.process.spawn:octet-pi-compat", trace["process"])
        self.assertNotIn("extensions.process.spawn:octet-pi-compat", trace["host"])


class TerminalTests(unittest.TestCase):
    def test_restored(self):
        stream = "\x1b[?2004h\x1b[?1000h\x1b[?1006h\x1b[>7u\x1b[?25l" + "\x1b[?2004l\x1b[?1000;1006l\x1b[<u\x1b[?25h"
        self.assertTrue(core.terminal_restore(stream.encode())["restored"])

    def test_leftovers(self):
        stream = "\x1b[?1049h\x1b[?2004h\x1b[?1006h\x1b[>1u\x1b[?25l\x1b[?2004l"
        result = core.terminal_restore(stream)
        self.assertFalse(result["restored"])
        joined = " ".join(result["leftover"])
        for needle in ("alt-screen", "mouse", "cursor hidden", "kitty"):
            self.assertIn(needle, joined)
        self.assertNotIn("bracketed", joined)

    def test_untouched_is_restored(self):
        self.assertTrue(core.terminal_restore(b"plain text")["restored"])


class IssueTests(unittest.TestCase):
    def test_blocks_and_lines(self):
        text = (
            "hello\n[Extension issues]\n  pkg conflicts with tool read\n\n  next line\n\n\nunrelated\n"
            "• [octet-pi-compat Warning] [Extension issues]:\n\n  /p/a.js\n   session_start callback skipped\n\n\n"
            "[pi-compat] skipped /p/x.ts: boom\nfine line\n"
        )
        issues = core.extract_issue_lines(text)
        self.assertTrue(any("conflicts with tool read" in item and "next line" in item for item in issues))
        self.assertTrue(any("session_start callback skipped" in item for item in issues))
        self.assertTrue(any("skipped /p/x.ts" in item for item in issues))
        self.assertFalse(any(item == "fine line" for item in issues))


class ParityTests(unittest.TestCase):
    def test_package_names(self):
        self.assertEqual(core.package_from_path("/a/node_modules/x/node_modules/@s/p/i.ts"), "@s/p")
        self.assertEqual(core.package_from_path("/a/node_modules/pkg/i.ts"), "pkg")
        self.assertIsNone(core.package_from_path("/a/pi-eval/sentinel.ts"))

    def test_octet_status(self):
        status = core.octet_package_status(BRIDGE, LOCKED, registered=[0, 1, 2, 3])
        self.assertEqual(status["pkg-a"]["status"], "loaded")
        self.assertEqual(status["@s/pkg-b"]["status"], "loaded")
        self.assertEqual(status["pkg-c"]["status"], "excluded")
        self.assertEqual(status["pkg-d"]["status"], "absent")

    def test_skipped_and_unregistered(self):
        text = "[pi-compat] skipped /h/.pi/agent/npm/node_modules/pkg-a/index.ts: bad import\n"
        status = core.octet_package_status(BRIDGE, LOCKED, skipped_text=text, registered=[0, 1])
        self.assertEqual(status["pkg-a"]["status"], "skipped")
        self.assertIn("bad import", status["pkg-a"]["detail"])
        status = core.octet_package_status(BRIDGE, LOCKED, registered=[0])
        self.assertEqual(status["@s/pkg-b"]["status"], "unregistered")
        # No trace at all falls back to bridge membership.
        self.assertEqual(core.octet_package_status(BRIDGE, LOCKED)["@s/pkg-b"]["status"], "loaded")

    def test_pi_status_conflict_still_loaded(self):
        status = core.pi_package_status(PROBE, LOCKED)
        self.assertEqual(status["pkg-c"]["status"], "loaded")
        self.assertIn("conflicts", status["pkg-c"]["detail"])
        self.assertEqual(status["pkg-d"]["status"], "absent")
        errored = core.pi_package_status({"extensions": [], "errors": PROBE["errors"]}, LOCKED)
        self.assertEqual(errored["pkg-c"]["status"], "error")

    def test_packages_parity(self):
        pi = core.pi_package_status(PROBE, LOCKED)
        octet = core.octet_package_status(BRIDGE, LOCKED)
        value, mismatched = core.packages_parity(pi, octet)
        self.assertEqual(mismatched, ["pkg-c"])  # Pi loads it, octet excluded it
        self.assertAlmostEqual(value, 0.75)
        self.assertEqual(core.packages_parity(pi, pi), (1.0, []))

    def test_commands(self):
        pi = core.pi_command_names(PROBE)
        octet = core.octet_command_names(BRIDGE)
        self.assertEqual(pi, {"alpha", "beta", "gamma"})  # sentinel excluded
        self.assertEqual(octet, {"alpha", "beta"})
        self.assertAlmostEqual(core.jaccard(pi, octet), 2 / 3)
        self.assertEqual(core.jaccard(set(), set()), 1.0)

    def test_peer_excluded(self):
        manifests = {"a": {"peerDependencies": {"@earendil-works/pi-tui": "^0.8", "other": "1"}}, "b": {"peerDependencies": {"@earendil-works/pi-tui": "*"}}}
        result = core.peer_excluded(manifests, {"@earendil-works/pi-tui": "1.4.0"}, lambda v, r: r == "*")
        self.assertEqual(list(result), ["a"])


class ReportTests(unittest.TestCase):
    @staticmethod
    def row(leg, target, rep, full, typeable=0.5, status="ok"):
        return {
            "prompt_id": core.case_id(target, leg), "rep": rep, "tags": [leg, target], "status": status,
            "grade": core.build_grade(target=target, full_ui_s=full, typeable_s=typeable, clean_exit=True, packages_parity_value=1.0, command_parity_value=1.0),
            "exit_s": 1.0,
        }

    def test_stats(self):
        self.assertEqual(core.median_iqr([5.0]), (5.0, 0.0))
        median, iqr = core.median_iqr([1, 2, 3, 4, 5])
        self.assertEqual((median, iqr), (3, 2.0))

    def test_aggregate_ratio_and_report(self):
        rows = [self.row("cold", "pi", i, v) for i, v in enumerate([20, 22, 24])]
        rows += [self.row("cold", "octet", i, v) for i, v in enumerate([30, 33, 36])]
        rows += [self.row("warm", "pi", 0, 5.0), self.row("warm", "octet", 0, 4.0)]
        rows.append(self.row("warm", "octet", 1, 99.0, status="truncated"))  # never counted
        agg = core.aggregate(rows)
        self.assertEqual(agg[("octet", "warm")]["full_ui_s"], [4.0])
        ratio = core.ratios(agg)
        self.assertAlmostEqual(ratio["cold"], 33 / 22)
        self.assertAlmostEqual(ratio["warm"], 0.8)
        report = core.format_report(agg, error_counts={"cold.octet:ceiling": 1})
        self.assertIn("1.50x", report)
        self.assertIn("0.80x", report)
        self.assertIn("cold.octet:ceiling=1", report)
        base = core.aggregate([self.row("cold", "pi", 0, 20), self.row("cold", "octet", 0, 40)])
        delta = core.format_report(agg, baseline=base)
        self.assertIn("full_ui_s=-7.000", delta)  # octet cold median 33 vs 40
        self.assertIn("2.00x -> 1.50x", delta)

    def test_state_document_headline_first_and_not_binary(self):
        state = core.state_document("baseline", ["x"])
        self.assertEqual(state["metrics"][0]["id"], "full_ui_s")
        self.assertEqual(state["metrics"][0]["better"], "lower")
        self.assertTrue(all(m["kind"] == "float" for m in state["metrics"]))
        self.assertEqual({m["id"] for m in state["metrics"]}, set(core.GRADE_METRICS))
        self.assertTrue(state["perf_fields"])


class ResumeTests(unittest.TestCase):
    def test_idempotent_keys_and_torn_lines(self):
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "results.jsonl"
            core.append_jsonl(path, ReportTests.row("cold", "pi", 0, 20))
            core.append_jsonl(path, {**ReportTests.row("cold", "octet", 0, 30), "status": "truncated"})
            with open(path, "a") as handle:
                handle.write('{"prompt_id": "torn')  # interrupted write
            self.assertEqual(core.completed_keys(path), {("cold.pi", 0)})
            self.assertEqual(len(core.load_jsonl(path)), 2)


if __name__ == "__main__":
    result = unittest.main(exit=False, verbosity=2).result
    print("\nselftest:", "PASS" if result.wasSuccessful() else "FAIL")
    raise SystemExit(0 if result.wasSuccessful() else 1)
