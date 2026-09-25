"""TASK 1 tests: the bounded `/subagents open-all <tmux|herdr>` escape hatch.

Every test patches the *process* environment, because the launcher resolves the
multiplexer and the `octet` binary with `shutil.which` on the real `PATH` (that
is the fail-closed discovery rule: installed or refuse, never download).
"""

from __future__ import annotations

import contextlib
import json
import os
import shutil
import stat
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path
from unittest import mock

try:
    from .helpers import owner
except ImportError:  # unittest discover -s tests
    from helpers import owner

from fake_agent_sessions import fake_session_reference
from octet_subagents.launcher import (
    ATOMIC_WRITER_CLAIM_BLOCKED_REASON,
    Pane,
    _pane_argv,
    DETACHED_WORKER_NOT_OPENED_REASON,
    MAX_OPEN_ALL_PANES,
    MULTIPLEXERS,
    PARKED_WORKER_NOT_OPENED_REASON,
    LaunchPlan,
    execute_plan,
    open_all,
    plan_argv_rows,
    plan_open_all,
    render_outcome,
    skipped_worker_row,
)
from octet_subagents.model import Worker
from octet_subagents.orchestrator import Orchestrator


PARENT_ID = "parent-session"
SECRETS = {
    "ANTHROPIC_API_KEY": "sk-ant-super-secret",
    "OPENAI_API_KEY": "sk-openai-super-secret",
    "OCTET_TOKEN": "bearer-super-secret",
}


def worker(state: str = "running", *, name: str = "explore-auth", index: int = 1, **values) -> Worker:
    options = dict(
        agent_id="agent-%d" % index,
        agent_path="/root/%s" % name,
        parent_id="root",
        depth=1,
        name=name,
        profile="explore",
        requested_model="inherit",
        effective_model="claude-sonnet-test",
        tools=("read", "search"),
        state=state,
        phase="searching",
        created_at_ms=1_700_000_000_000,
        started_at_ms=1_700_000_000_000,
        deadline_at_ms=None,
        timeout_seconds=None,
        max_turns=None,
        max_output_bytes=8192,
        max_tokens=None,
        max_cost_microdollars=None,
        session=fake_session_reference("agent-%d" % index),
        generation=1,
    )
    options.update(values)
    return Worker(**options)


class Stub:
    """A temporary directory holding stub `tmux`/`herdr`/`octet` executables."""

    def __init__(self) -> None:
        self.directory = tempfile.TemporaryDirectory()
        self.calls_path = os.path.join(self.directory.name, "calls.jsonl")
        self._write(
            "tmux",
            "#!/usr/bin/env python3\n"
            "import json, os, sys\n"
            "with open(os.environ['STUB_CALLS'], 'a', encoding='utf-8') as handle:\n"
            "    handle.write(json.dumps(sys.argv[1:]) + '\\n')\n"
            "count = sum(1 for _ in open(os.environ['STUB_CALLS'], encoding='utf-8'))\n"
            "if os.environ.get('STUB_FAIL_ON') == str(count):\n"
            "    sys.stderr.write('stub failure\\n')\n"
            "    raise SystemExit(1)\n"
            "if sys.argv[1:] == ['list-sessions', '-F', '#{session_name}']:\n"
            "    print(os.environ.get('STUB_SESSIONS', ''))\n",
        )
        self.tmux = os.path.join(self.directory.name, "tmux")
        self._write(
            "herdr",
            "#!/usr/bin/env python3\n"
            "import json, os, sys\n"
            "with open(os.environ['STUB_CALLS'], 'a', encoding='utf-8') as handle:\n"
            "    handle.write(json.dumps(sys.argv[1:]) + '\\n')\n"
            "if sys.argv[1:3] == ['pane', 'split']:\n"
            "    print(json.dumps({'result': {'pane': {'pane_id': '%7'}}}))\n",
        )
        self.herdr = os.path.join(self.directory.name, "herdr")
        # A stand-in for the `octet` binary that stays alive, so a real tmux
        # session does not die the moment the pane command exits.
        self._write("octet", "#!/usr/bin/env python3\nimport time\ntime.sleep(120)\n")

    def _write(self, name: str, body: str) -> None:
        path = Path(self.directory.name) / name
        path.write_text(body, encoding="utf-8")
        path.chmod(path.stat().st_mode | stat.S_IXUSR | stat.S_IXGRP | stat.S_IXOTH)

    @contextlib.contextmanager
    def path(self, **values):
        environment = {
            "PATH": "%s%s%s" % (self.directory.name, os.pathsep, os.environ.get("PATH", "")),
            "STUB_CALLS": self.calls_path,
            "STUB_SESSIONS": "",
            "OCTET_SUBAGENTS_OCTET_BIN": os.path.join(self.directory.name, "octet"),
        }
        environment.update(SECRETS)
        environment.update(values)
        removed = [key for key, value in environment.items() if value is None]
        for key in removed:
            del environment[key]
        with mock.patch.dict(os.environ, environment):
            for key in removed:
                os.environ.pop(key, None)
            yield

    def recorded(self) -> list[list[str]]:
        if not os.path.exists(self.calls_path):
            return []
        with open(self.calls_path, encoding="utf-8") as handle:
            return [json.loads(line) for line in handle if line.strip()]

    def close(self) -> None:
        self.directory.cleanup()


class RefusalTests(unittest.TestCase):
    def test_open_all_without_a_multiplexer_refuses_and_spawns_nothing(self):
        """Fail closed: no multiplexer on PATH -> refuse, never install, never run."""
        empty = tempfile.TemporaryDirectory()
        spawns: list[list[str]] = []
        real_run = subprocess.run

        def recording_run(argv, *args, **kwargs):
            spawns.append(list(argv) if isinstance(argv, (list, tuple)) else [argv])
            return real_run(argv, *args, **kwargs)

        try:
            with mock.patch.dict(os.environ, {"PATH": empty.name, "HERDR_ENV": "1"}):
                os.environ.pop("OCTET_SUBAGENTS_OCTET_BIN", None)
                self.assertIsNone(shutil.which("tmux"))
                self.assertIsNone(shutil.which("herdr"))
                subprocess.run = recording_run  # type: ignore[assignment]
                for name in MULTIPLEXERS:
                    with self.subTest(multiplexer=name):
                        with self.assertRaises(Exception) as raised:
                            open_all(
                                multiplexer=name,
                                parent_session_id=PARENT_ID,
                                workers=[worker()],
                                workspace=None,
                            )
                        self.assertEqual(
                            getattr(raised.exception, "code", None), "multiplexer_missing"
                        )
                        self.assertIn(name, str(raised.exception))
                        self.assertIn("never", str(raised.exception))
        finally:
            subprocess.run = real_run  # type: ignore[assignment]
            empty.cleanup()
        self.assertEqual(
            spawns, [], "open-all must not run anything when the multiplexer is missing"
        )

    def test_unknown_multiplexer_is_refused(self):
        stub = Stub()
        try:
            with stub.path():
                with self.assertRaises(Exception) as raised:
                    plan_open_all(
                        multiplexer="screen",
                        parent_session_id=PARENT_ID,
                        workers=[worker()],
                        workspace=None,
                    )
        finally:
            stub.close()
        self.assertEqual(getattr(raised.exception, "code", None), "unsupported_multiplexer")

    def test_session_id_with_shell_metacharacters_is_rejected(self):
        """A session id can never widen a command line."""
        stub = Stub()
        try:
            with stub.path(TMUX=None):
                for hostile in (
                    "parent;rm -rf /",
                    "parent$(touch /tmp/pwned)",
                    "parent`id`",
                    "parent&&id",
                    "parent|cat /etc/passwd",
                    "parent\nnew-session",
                    "parent a",
                ):
                    with self.subTest(session=hostile):
                        with self.assertRaises(Exception) as raised:
                            plan_open_all(
                                multiplexer="tmux",
                                parent_session_id=hostile,
                                workers=[worker()],
                                workspace=None,
                            )
                        self.assertEqual(
                            getattr(raised.exception, "code", None), "unlaunchable_session"
                        )
        finally:
            stub.close()
        self.assertEqual(stub.recorded(), [])

    def test_worker_handle_must_be_an_opaque_reference(self):
        stub = Stub()
        try:
            with stub.path(TMUX=None):
                for hostile in (
                    "/Users/someone/.octet/sessions/team-x/child.jsonl",
                    "agent-session:not-a-digest",
                    "agent-session:" + "g" * 64,
                    "agent-session:%s;id" % ("a" * 62),
                    "agent-session:%s $(id)" % ("a" * 54),
                ):
                    with self.subTest(reference=hostile):
                        with self.assertRaises(Exception) as raised:
                            plan_open_all(
                                multiplexer="tmux",
                                parent_session_id=PARENT_ID,
                                workers=[worker(session=hostile)],
                                workspace=None,
                            )
                        self.assertEqual(
                            getattr(raised.exception, "code", None), "unlaunchable_session"
                        )
        finally:
            stub.close()
        self.assertEqual(stub.recorded(), [])

    def test_pane_cap_is_enforced_before_anything_is_created(self):
        stub = Stub()
        try:
            with stub.path(TMUX=None):
                with self.assertRaises(Exception) as raised:
                    plan_open_all(
                        multiplexer="tmux",
                        parent_session_id=PARENT_ID,
                        workers=[worker(index=index) for index in range(MAX_OPEN_ALL_PANES)],
                        workspace=None,
                    )
                self.assertEqual(getattr(raised.exception, "code", None), "pane_cap")
                self.assertIn(str(MAX_OPEN_ALL_PANES), str(raised.exception))
        finally:
            stub.close()
        self.assertEqual(stub.recorded(), [])

    def test_missing_octet_binary_is_refused_not_downloaded(self):
        stub = Stub()
        only = tempfile.TemporaryDirectory()
        try:
            for name in ("tmux", "herdr"):
                target = os.path.join(only.name, name)
                os.symlink(getattr(stub, name), target)
            path = "%s%s%s" % (
                only.name,
                os.pathsep,
                os.path.dirname(os.path.realpath(sys.executable)),
            )
            os.environ.pop("TMUX", None)
            with mock.patch.dict(
                os.environ,
                {"PATH": path, "HERDR_ENV": "1"},
            ):
                os.environ.pop("OCTET_SUBAGENTS_OCTET_BIN", None)
                if shutil.which("octet") is not None:
                    self.skipTest("a real octet binary is on PATH; cannot test the missing case")
                with self.assertRaises(Exception) as raised:
                    plan_open_all(
                        multiplexer="tmux",
                        parent_session_id=PARENT_ID,
                        workers=[worker()],
                        workspace=None,
                    )
        finally:
            only.cleanup()
            stub.close()
        self.assertEqual(getattr(raised.exception, "code", None), "octet_missing")
        self.assertIn("never", str(raised.exception))
        self.assertEqual(stub.recorded(), [])


def ready_worker(**values):
    return worker("done", launchable=True, live_task=False, **values)


def adapter_plan(octet_binary, workers, multiplexer="tmux", workspace="/workspace"):
    """Direct low-level executor fixture; never product launch authorization."""
    panes = tuple(
        Pane(role="worker", name=item.name, handle_kind="opaque_session_reference",
             handle=item.session, resolvable=True,
             argv=_pane_argv(multiplexer=multiplexer, octet_binary=octet_binary,
                             handle=item.session, label=item.name, workspace=workspace,
                             inside=False, session_name="octet-fleet-parent-session",
                             first=index == 0))
        for index, item in enumerate(workers)
    )
    return LaunchPlan(multiplexer=multiplexer, binary=multiplexer, panes=panes,
                      workspace=workspace, session_name="octet-fleet-parent-session",
                      inside_multiplexer=False)


class LauncherTestCase(unittest.TestCase):
    def setUp(self):
        self.stub = Stub()
        self.addCleanup(self.stub.close)
        self.environment = self.stub.path(TMUX=None, HERDR_ENV="1")
        self.environment.__enter__()
        self.addCleanup(self.environment.__exit__, None, None, None)

    def plan(self, workers=(), multiplexer="tmux", workspace="/workspace"):
        return plan_open_all(multiplexer=multiplexer, parent_session_id=PARENT_ID,
                             workers=workers, workspace=workspace)


class PlanTests(LauncherTestCase):
    def test_parent_and_launchable_worker_stay_blocked_with_opaque_handle(self):
        plan = self.plan([ready_worker(index=2)])
        self.assertFalse(plan.panes[0].resolvable)
        self.assertIn("second writer", plan.panes[0].blocked_reason)
        self.assertFalse(plan.panes[1].resolvable)
        self.assertEqual(plan.panes[1].blocked_reason, ATOMIC_WRITER_CLAIM_BLOCKED_REASON)
        self.assertEqual(plan.panes[1].handle_kind, "opaque_session_reference")
        self.assertEqual(plan.panes[1].argv, (
            "tmux", "new-session", "-d", "-s", "octet-fleet-parent-session",
            "-n", "explore-auth", "-c", "/workspace", "--",
            self.stub.directory.name + "/octet", "--resume", fake_session_reference("agent-2")))

    def test_inside_tmux_uses_new_window(self):
        with mock.patch.dict(os.environ, {"TMUX": "fixture"}):
            plan = self.plan([ready_worker()])
        self.assertEqual(plan.panes[1].argv[:3], ("tmux", "new-window", "-d"))
        self.assertIsNone(plan.session_name)

    def test_host_launchability_does_not_grant_writer_ownership(self):
        plan = self.plan([ready_worker(index=1),
                          worker("orphaned", index=2, launchable=True, live_task=False),
                          worker(index=3, launchable=True, live_task=True),
                          worker("awaiting_approval", index=4, launchable=True, live_task=False)])
        self.assertEqual(plan.executable, ())
        self.assertEqual(plan.panes[1].blocked_reason, ATOMIC_WRITER_CLAIM_BLOCKED_REASON)
        self.assertEqual(plan.panes[2].blocked_reason, ATOMIC_WRITER_CLAIM_BLOCKED_REASON)
        self.assertIn("live worker", plan.panes[3].blocked_reason)
        self.assertIn("approval", plan.panes[4].blocked_reason)
        self.assertEqual(plan.panes[2].argv[1], "new-window")

    def test_launchability_fails_closed_on_missing_or_conflicting_fields(self):
        for values in ({}, {"launchable": True}, {"launchable": 1, "live_task": False},
                       {"launchable": True, "live_task": False, "host_present": False},
                       {"launchable": True, "live_task": False, "launch_blocked": "transcript gone"}):
            with self.subTest(values=values):
                plan = self.plan([worker(**values)])
                self.assertEqual(plan.executable, ())
        plan = self.plan([worker(launch_blocked="transcript gone")])
        self.assertEqual(plan.panes[1].blocked_reason, "transcript gone")

    def test_duplicate_handles_are_refused_before_effect(self):
        with self.assertRaises(Exception) as raised:
            self.plan([ready_worker(index=1), ready_worker(index=1, name="duplicate")])
        self.assertEqual(raised.exception.code, "invalid_launch")
        self.assertEqual(self.stub.recorded(), [])

    def test_herdr_requires_ownership(self):
        with mock.patch.dict(os.environ, {"HERDR_ENV": "0"}):
            with self.assertRaises(Exception) as raised:
                self.plan([ready_worker()], multiplexer="herdr")
        self.assertEqual(raised.exception.code, "multiplexer_not_owner")

    def test_launchable_snapshot_alone_has_zero_pane_effects_on_repeated_open_all(self):
        for multiplexer in MULTIPLEXERS:
            for attempt in range(2):
                with self.subTest(multiplexer=multiplexer, attempt=attempt):
                    with mock.patch("octet_subagents.launcher._run") as run:
                        plan, outcome = open_all(
                            multiplexer=multiplexer, parent_session_id=PARENT_ID,
                            workers=[ready_worker()], workspace="/workspace")
                    run.assert_not_called()
                    self.assertEqual(plan.executable, ())
                    self.assertEqual(outcome.created, [])
                    self.assertEqual(len(outcome.blocked), 2)
                    self.assertIn("atomic host writer claim/settlement unavailable",
                                  render_outcome(plan, outcome))
        self.assertEqual(self.stub.recorded(), [])

    def test_blocked_herdr_plan_preserves_validated_workspace(self):
        with mock.patch.dict(os.environ, {"OCTET_WORKSPACE": "/workspace"}):
            plan = self.plan([ready_worker()], multiplexer="herdr", workspace=None)
        self.assertEqual(plan.workspace, "/workspace")
        self.assertEqual(plan.executable, ())

    def test_no_credentials_in_plan_or_outcome(self):
        plan = self.plan([ready_worker()])
        outcome = execute_plan(plan)
        text = render_outcome(plan, outcome)
        blob = json.dumps(plan_argv_rows(plan)) + text
        for secret in SECRETS.values():
            self.assertNotIn(secret, blob)
        self.assertEqual(outcome.created, [])
        self.assertIn("blocked (Partial)", text)
        self.assertEqual(self.stub.recorded(), [])

    def test_skipped_detached_and_parked_workers_keep_actionable_reasons(self):
        detached = skipped_worker_row(worker("orphaned", host_present=False))
        parked = skipped_worker_row(worker("awaiting_approval"))
        self.assertEqual(detached["reason"], DETACHED_WORKER_NOT_OPENED_REASON)
        self.assertEqual(parked["reason"], PARKED_WORKER_NOT_OPENED_REASON)
        self.assertTrue(detached["reattachable"])
        self.assertFalse(parked["reattachable"])


class AdapterExecutionTests(LauncherTestCase):
    """Exercise adapters directly without weakening product ownership checks."""

    def adapter_plan(self, workers, multiplexer="tmux"):
        return adapter_plan(self.stub.directory.name + "/octet", workers, multiplexer)

    def test_tmux_adapter_creates_session_then_window(self):
        plan = self.adapter_plan([ready_worker(index=1), ready_worker(index=2)])
        outcome = execute_plan(plan)
        self.assertTrue(outcome.ok)
        self.assertEqual(len(outcome.created), 2)
        self.assertEqual(self.stub.recorded(), [list(pane.argv[1:]) for pane in plan.panes])
        self.assertEqual(plan.panes[0].argv[1], "new-session")
        self.assertEqual(plan.panes[1].argv[1], "new-window")
        self.assertIn("interactive startup is not confirmed", render_outcome(plan, outcome))

    def test_herdr_unsafe_binary_is_refused_before_split(self):
        plan = adapter_plan("/app dir/octet", [ready_worker()], "herdr")
        with self.assertRaises(Exception) as raised:
            execute_plan(plan)
        self.assertEqual(raised.exception.code, "unsafe_command_token")
        self.assertEqual(self.stub.recorded(), [])

    def test_herdr_split_and_submit_use_workspace_and_opaque_handle(self):
        plan = self.adapter_plan([ready_worker()], multiplexer="herdr")
        outcome = execute_plan(plan)
        self.assertTrue(outcome.ok)
        self.assertEqual(self.stub.recorded(), [
            ["pane", "split", "--current", "--direction", "right", "--no-focus", "--cwd", "/workspace"],
            ["pane", "run", "%7", self.stub.directory.name + "/octet --resume " + fake_session_reference("agent-1")]])
        self.assertEqual(outcome.created[0]["pane_id"], "%7")

    def test_failure_stops_before_next_worker(self):
        plan = self.adapter_plan([ready_worker(index=1), ready_worker(index=2)])
        with mock.patch.dict(os.environ, {"STUB_FAIL_ON": "1"}):
            outcome = execute_plan(plan)
        self.assertFalse(outcome.ok)
        self.assertEqual(outcome.created, [])
        self.assertEqual(len(self.stub.recorded()), 1)
        self.assertIn("inspect", render_outcome(plan, outcome))

    def test_herdr_malformed_split_reports_possible_orphan_without_submission(self):
        plan = self.adapter_plan([ready_worker()], multiplexer="herdr")
        for stdout in ("not json", "null", '{"result":null}', '{"result":[]}',
                       '{"result":{"pane":{"pane_id":"unsafe;id"}}}'):
            with self.subTest(stdout=stdout):
                with mock.patch("octet_subagents.launcher._run", return_value=subprocess.CompletedProcess([], 0, stdout, "")) as run:
                    outcome = execute_plan(plan)
                self.assertFalse(outcome.ok)
                self.assertEqual(run.call_count, 1)
                self.assertIsNone(outcome.failure["pane_created"])
                self.assertIn("may exist", outcome.failure["stderr"])
                self.assertNotIn("nothing was opened", render_outcome(plan, outcome))

    def test_herdr_run_failure_or_timeout_retains_created_pane_id(self):
        plan = self.adapter_plan([ready_worker(), ready_worker(index=2)], multiplexer="herdr")
        split = subprocess.CompletedProcess([], 0, '{"result":{"pane":{"pane_id":"%7"}}}', "")
        for failure in (subprocess.CompletedProcess([], 1, "", "submission failed"),
                        subprocess.TimeoutExpired("herdr", 15), OSError("fixture")):
            with self.subTest(failure=failure):
                with mock.patch("octet_subagents.launcher._run", side_effect=[split, failure]) as run:
                    outcome = execute_plan(plan)
                self.assertFalse(outcome.ok)
                self.assertEqual(run.call_count, 2)
                self.assertEqual(len(outcome.created), 1)
                self.assertEqual(outcome.failure["pane_id"], "%7")
                self.assertIsNone(outcome.failure["command_submitted"])
                self.assertIn("id %7", render_outcome(plan, outcome))

    def test_herdr_split_timeout_is_ambiguous(self):
        plan = self.adapter_plan([ready_worker()], multiplexer="herdr")
        with mock.patch("octet_subagents.launcher._run", side_effect=subprocess.TimeoutExpired("herdr", 15)):
            outcome = execute_plan(plan)
        self.assertFalse(outcome.ok)
        self.assertIsNone(outcome.failure["pane_created"])
        self.assertEqual(outcome.created, [])

    def test_open_all_reports_blocked_parent_and_skipped_unlaunchable_worker(self):
        result = Orchestrator(publish=lambda _: None).open_all(
            owner=owner(), workers=[ready_worker(), worker("orphaned", host_present=False, index=2)],
            arguments=["tmux"])
        self.assertEqual(len(result["skipped"]), 1)
        self.assertIn("blocked parent", result["text"])
        self.assertIn("atomic host writer claim/settlement unavailable", result["text"])
        self.assertIn("unchanged", result["text"])
        self.assertEqual(self.stub.recorded(), [])


@unittest.skipUnless(shutil.which("tmux"), "tmux is not installed on this host")
class RealTmuxTests(unittest.TestCase):
    def test_real_tmux_creates_a_detached_session_then_reaps_it(self):
        """Guarded on availability; the created session is destroyed in teardown."""
        stub = Stub()
        session = "octet-openall-real-%d" % os.getpid()
        try:
            with stub.path(TMUX=None):
                plan = adapter_plan(stub.directory.name + "/octet", [ready_worker()],
                                    workspace=None)
            parent = plan.executable[0]
            argv = list(parent.argv)
            argv[4] = session  # use the guarded session name
            try:
                created = subprocess.run(argv, capture_output=True, text=True, timeout=20)
                self.assertEqual(created.returncode, 0, created.stderr)
                listing = subprocess.run(
                    ["tmux", "list-sessions", "-F", "#{session_name}"],
                    capture_output=True,
                    text=True,
                    timeout=20,
                )
                self.assertIn(session, listing.stdout)
            finally:
                subprocess.run(
                    ["tmux", "kill-session", "-t", session],
                    capture_output=True,
                    text=True,
                    timeout=20,
                )
            after = subprocess.run(
                ["tmux", "list-sessions", "-F", "#{session_name}"],
                capture_output=True,
                text=True,
                timeout=20,
            )
            self.assertNotIn(session, after.stdout)
        finally:
            stub.close()


if __name__ == "__main__":
    unittest.main()
