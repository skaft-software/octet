"""Synthetic long-session benchmark contracts; no binary, PTY, or network is used."""
import contextlib
import importlib.util
import io
import json
from pathlib import Path
import statistics
import sys
import tempfile
import time
import unittest
from unittest.mock import patch

SPEC = importlib.util.spec_from_file_location(
    "long_session_bench", Path(__file__).resolve().parents[2] / "benchmarks/long_session_bench.py")
sys.path.insert(0, str(Path(__file__).resolve().parents[2] / "benchmarks"))
BENCH = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(BENCH)

REQUEST = {"model": BENCH.MODEL, "stream": True}


def request(messages, **overrides):
    return dict(REQUEST, messages=messages, **overrides)


def plan(fixture, messages, **overrides):
    return fixture.plan(request(messages, **overrides), 0, len(json.dumps(messages)), 0.0)


class Wire:
    """The provider-visible conversation a correct octet would send, turn by turn."""

    def __init__(self):
        self.messages = [{"role": "system", "content": "Deterministic offline fixture."}]

    def ask(self, index):
        self.messages.append({"role": "user", "content": f"LONG_PROMPT_{index:05}"})
        return list(self.messages)

    def answer(self, index, streaming=False):
        self.messages.append({"role": "assistant", "content": BENCH.answer(index, streaming)})

    def tool(self, index, output=None):
        self.messages.append({"role": "assistant", "content": "", "tool_calls": [{
            "id": f"call-{index}", "type": "function",
            "function": {"name": "bash", "arguments": json.dumps({"command": "cat bench-large.txt"})}}]})
        self.messages.append({"role": "tool", "tool_call_id": f"call-{index}",
                              "content": "exit=0\n" + BENCH.LARGE_DATA if output is None else output})


def advance(fixture, wire, index):
    """One complete turn: the prompt request, plus the tool exchange when due."""
    _, calls = plan(fixture, wire.ask(index))
    if calls:
        wire.tool(index)
        plan(fixture, list(wire.messages))
    else:
        wire.answer(index, index in fixture.scales)
    return calls


class FixtureContractTests(unittest.TestCase):
    def test_complete_turns_are_accepted_and_counted(self):
        fixture = BENCH.Fixture([2])
        wire = Wire()
        self.assertEqual([advance(fixture, wire, i) for i in (1, 2)], [False, False])
        self.assertEqual(fixture.next_turn, 3)
        self.assertEqual([row["turn"] for row in fixture.rows], [1, 2])
        # The fixture retains what the last request carried: every message but
        # the answer that no later request has replayed yet.
        self.assertEqual(fixture.messages, wire.messages[:-1])

    def test_every_request_must_replay_the_validated_history_exactly(self):
        fixture = BENCH.Fixture([])
        wire = Wire()
        advance(fixture, wire, 1)
        advance(fixture, wire, 2)
        accepted = list(wire.messages)
        dropped = accepted[:-1] + [{"role": "user", "content": "LONG_PROMPT_00003"}]
        rewritten = [dict(message) for message in accepted]
        rewritten[2] = {"role": "assistant", "content": "condensed summary"}
        reordered = [accepted[0], accepted[2], accepted[1], {"role": "user", "content": "LONG_PROMPT_00003"}]
        for label, messages in (
            ("dropped", dropped),
            ("rewritten", rewritten),
            ("reordered", reordered),
            ("missing new prompt", accepted),
            ("duplicated prompt", accepted + accepted[1:2]),
        ):
            with self.subTest(label=label), self.assertRaises(AssertionError):
                plan(fixture, messages)
        self.assertEqual(fixture.next_turn, 3)
        plan(fixture, accepted + [{"role": "user", "content": "LONG_PROMPT_00003"}])
        self.assertEqual(fixture.next_turn, 4)

    def test_scale_boundary_answer_must_match_the_streamed_text(self):
        fixture = BENCH.Fixture([1])
        wire = Wire()
        _, calls = plan(fixture, wire.ask(1))
        self.assertFalse(calls)
        wire.answer(1, streaming=False)
        with self.assertRaisesRegex(AssertionError, "completed answer changed"):
            plan(fixture, wire.ask(2))

    def test_large_tool_result_must_be_complete_and_paired(self):
        fixture = BENCH.Fixture([])
        wire = Wire()
        for index in range(1, BENCH.LARGE_EVERY):
            advance(fixture, wire, index)
        prompt = wire.ask(BENCH.LARGE_EVERY)
        _, calls = plan(fixture, prompt)
        self.assertTrue(calls)
        self.assertEqual(fixture.rows[-1]["tool_call"], True)
        wire.tool(BENCH.LARGE_EVERY)
        complete = list(wire.messages)
        incomplete = json.loads(json.dumps(complete))
        incomplete[-1]["content"] = incomplete[-1]["content"][:1000]
        unpaired = json.loads(json.dumps(complete))
        unpaired[-1]["tool_call_id"] = "call-someone-else"
        wrong_arguments = json.loads(json.dumps(complete))
        wrong_arguments[-2]["tool_calls"][0]["function"]["arguments"] = json.dumps({"command": "cat other.txt"})
        missing_call = json.loads(json.dumps(complete))
        missing_call[-2].pop("tool_calls")
        for label, messages in (("truncated", incomplete), ("unpaired", unpaired),
                                ("arguments", wrong_arguments), ("missing call", missing_call)):
            with self.subTest(label=label), self.assertRaises(AssertionError):
                plan(fixture, messages)

    def test_unexpected_request_shapes_are_refused(self):
        fixture = BENCH.Fixture([])
        wire = Wire()
        advance(fixture, wire, 1)
        for label, overrides in (("model", {"model": "other-model"}), ("streaming", {"stream": False})):
            with self.subTest(label=label), self.assertRaisesRegex(AssertionError, "unexpected model"):
                plan(fixture, wire.ask(2), **overrides)
        with self.assertRaisesRegex(AssertionError, "previous answer or new prompt missing"):
            # A retry that resends the accepted prompt without new output.
            plan(fixture, fixture.messages)
        with self.assertRaisesRegex(AssertionError, "unexpected request"):
            # Discovery, a retry, or any exchange that adds no new user prompt.
            plan(BENCH.Fixture([]), [{"role": "system", "content": "s"},
                                     {"role": "assistant", "content": "probe"}])

    def test_turn_count_and_wire_size_are_recorded(self):
        fixture = BENCH.Fixture([])
        wire = Wire()
        advance(fixture, wire, 1)
        row = fixture.rows[-1]
        self.assertEqual(set(row), {"turn", "tool_call", "received_ns", "wire_bytes", "decode_ms", "validated_ns"})
        self.assertGreater(row["wire_bytes"], 0)


class SummaryTests(unittest.TestCase):
    def test_percentiles_interpolate_and_require_samples(self):
        self.assertEqual(BENCH.percentile([1, 2, 3, 4], .5), 2.5)
        self.assertAlmostEqual(BENCH.percentile([1, 2, 3, 4], .95), 3.85)
        self.assertEqual(BENCH.distribution([4]), {"count": 1, "p50": 4, "p95": 4, "max": 4})
        with self.assertRaises(ValueError):
            BENCH.distribution([])

    def test_rss_growth_is_a_per_100_turn_slope(self):
        rows = [{"turn": turn, "rss_bytes": 2_000_000 + 4096 * turn} for turn in range(1, 51)]
        self.assertAlmostEqual(BENCH.growth_per_100(rows), 409_600)
        with self.assertRaises(ValueError):
            BENCH.growth_per_100([{"turn": 7, "rss_bytes": 1}])

    def test_summary_uses_ordinary_turns_and_the_last_stream_cell(self):
        samples = [{"turn": turn, "kind": "stream" if turn == 60 else ("large_tool" if turn % 25 == 0 else "small"),
                    "wall_ms": float(turn), "reply_frame_ms": float(turn) - 40.0,
                    "settled_ms": float(turn) - 40.0 + .5,
                    "append_ms": None if turn == 1 else .5, "peak_rss_bytes": 10 + turn,
                    "rss_bytes": 100 + turn, "session_bytes": 1000 * turn, "append_bytes": 10,
                    "cpu_ms": 1.0, "provider_validation_ms": .1} for turn in range(1, 61)]
        summary = BENCH.summarize(samples)
        ordinary = [turn for turn in range(1, 60) if turn not in (25, 50)]
        self.assertEqual(summary["turn_latency_ms"]["count"], 60)
        self.assertEqual(summary["early_small_p50_ms"], statistics.median(ordinary[:25]))
        self.assertEqual(summary["late_small_p50_ms"], statistics.median(ordinary[-25:]))
        self.assertEqual(summary["late_small_reply_p50_ms"], statistics.median(ordinary[-25:]) - 40.0)
        self.assertEqual(summary["settled_ms"]["count"], 60)
        self.assertEqual(summary["late_small_settled_p50_ms"], statistics.median(ordinary[-25:]) - 39.5)
        self.assertEqual(summary["stream"]["turn"], 60)
        self.assertEqual(summary["append_persist_ms_per_turn"]["count"], 59)
        self.assertAlmostEqual(summary["rss_growth_bytes_per_100_turns"], 100)


def session_records(turns, large_every=BENCH.LARGE_EVERY, scales=()):
    """A durable ledger shaped like the fixture's own completed turns."""
    records, next_id = [], 1
    parent = None

    def entry(value):
        nonlocal parent, next_id
        identifier = str(next_id)
        next_id += 1
        records.append({"type": "entry", "id": identifier, "parent": parent, "value": value})
        parent = identifier
        return identifier

    for index in range(1, turns + 1):
        prompt = entry({"type": "message", "User": {"content": [{"Text": f"LONG_PROMPT_{index:05}"}]}})
        if index % large_every == 0:
            entry({"type": "message", "Assistant": {"content": [{"ToolCall": {"id": f"call-{index}"}}]}})
            entry({"type": "message", "User": {"content": [{"ToolResult": {
                "tool_call_id": f"call-{index}", "is_error": False,
                "content": [{"Text": "exit=0\n" + BENCH.LARGE_DATA}]}}]}})
        entry({"type": "message", "Assistant": {"content": [
            {"Text": BENCH.answer(index, index in scales)}]}})
        records.append({"type": "checkpoint", "prompt": prompt, "head": parent})
        records.append({"type": "head", "id": parent})
    return records


def write_session(directory, records):
    path = Path(directory) / "session.jsonl"
    path.write_text("".join(json.dumps(record) + "\n" for record in records))
    return path


class DurableSessionTests(unittest.TestCase):
    def test_complete_ledger_is_accepted(self):
        with tempfile.TemporaryDirectory() as directory:
            path = write_session(directory, session_records(BENCH.LARGE_EVERY + 1))
            self.assertEqual(BENCH.check_session(path, BENCH.LARGE_EVERY + 1, ()),
                             {"users": 26, "answers": 26, "large_tools": 1, "checkpoints": 26})

    def test_tampering_is_detected(self):
        def broken(records, edit):
            copy = json.loads(json.dumps(records))
            edit(copy)
            return copy

        def entries(copy):
            return [record for record in copy if record["type"] == "entry"]

        def truncate(copy):
            for record in entries(copy):
                for part in record["value"].get("User", {}).get("content", []):
                    if "ToolResult" in part:
                        part["ToolResult"]["content"][0]["Text"] = "exit=0"
                        return
            raise AssertionError("fixture has no tool result to truncate")

        def reparent(copy):
            entries(copy)[2]["parent"] = "missing-entry"

        def drop_record(copy):
            copy.pop(copy.index(entries(copy)[1]))

        def wrong_head(copy):
            head = next(record for record in copy if record["type"] == "head")
            head["id"] = entries(copy)[0]["id"]

        def rewrite_prompt(copy):
            entries(copy)[0]["value"]["User"]["content"][0]["Text"] = "LONG_PROMPT_99999"

        def drop_checkpoint(copy):
            copy[:] = [record for record in copy if record["type"] != "checkpoint"]

        turns = BENCH.LARGE_EVERY + 1
        cases = {"truncated tool output": truncate, "reparented": reparent, "dropped entry": drop_record,
                 "wrong head": wrong_head, "rewritten prompt": rewrite_prompt, "missing checkpoints": drop_checkpoint}
        for label, edit in cases.items():
            with self.subTest(label=label), tempfile.TemporaryDirectory() as directory:
                with self.assertRaises(AssertionError):
                    BENCH.check_session(write_session(directory, broken(session_records(turns), edit)), turns, ())


class LedgerTests(unittest.TestCase):
    def test_checkpoints_are_counted_incrementally(self):
        with tempfile.TemporaryDirectory() as directory:
            path = write_session(directory, session_records(2))
            ledger = BENCH.Ledger(path)
            try:
                self.assertEqual(ledger.advance(), 2)
                ledger.wait_checkpoints(2, 1)
                self.assertEqual(ledger.checkpoints, 2)
                # Appending a torn line must not be counted or raise.
                with path.open("ab") as stream:
                    stream.write(b'{"type": "checkpoint"')
                self.assertEqual(ledger.advance(), 2)
                with path.open("ab") as stream:
                    stream.write(b', "prompt": "9", "head": "9"}\n')
                self.assertEqual(ledger.advance(), 3)
            finally:
                ledger.close()

    def test_a_missing_checkpoint_fails_within_the_bound(self):
        with tempfile.TemporaryDirectory() as directory:
            ledger = BENCH.Ledger(write_session(directory, session_records(1)))
            try:
                started = time.monotonic()
                with self.assertRaisesRegex(TimeoutError, "did not settle: 1 durable checkpoints"):
                    ledger.wait_checkpoints(2, .05)
                self.assertLess(time.monotonic() - started, 1)
            finally:
                ledger.close()


class TraceTests(unittest.TestCase):
    def test_payload_free_trace_lines_are_parsed_and_other_output_ignored(self):
        raw = (b"warning: unrelated\n"
               b"octet-perf: session.persist elapsed=1234us bytes=99\n"
               b"octet-perf: session.persist elapsed=7us bytes=1\n"
               b"panic text that is not a trace\n")
        self.assertEqual(BENCH.traces(raw), {"session.persist": [
            {"wall_ms": 1.234, "bytes": 99}, {"wall_ms": 0.007, "bytes": 1}]})
        self.assertEqual(BENCH.traces(b"noise\n"), {})


class CommandLineTests(unittest.TestCase):
    def main_with(self, *arguments):
        stderr = io.StringIO()
        with patch.object(sys, "argv", ["long_session_bench.py", *arguments]), \
                contextlib.redirect_stderr(stderr):
            with self.assertRaises(SystemExit) as raised:
                BENCH.main()
        return raised.exception.code, stderr.getvalue()

    def test_unbounded_unsorted_and_trivial_scales_are_refused(self):
        for scales in (["1000", "100"], ["5"], ["6000"], ["100", "100"]):
            with self.subTest(scales=scales):
                code, _ = self.main_with("--binary", "unused", "--output", "unused", "--scales", *scales)
                self.assertEqual(code, 2)

    def test_nonpositive_or_infinite_timeout_is_refused(self):
        for timeout in ("0", "-1", "inf"):
            with self.subTest(timeout=timeout):
                code, _ = self.main_with("--binary", "unused", "--output", "unused", "--timeout", timeout)
                self.assertEqual(code, 2)


if __name__ == "__main__":
    unittest.main()
