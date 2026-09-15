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
    MAX_OPEN_ALL_PANES,
    MULTIPLEXERS,
    LaunchPlan,
    execute_plan,
    open_all,
    plan_argv_rows,
    plan_open_all,
    render_outcome,
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


class PlanTests(unittest.TestCase):
    def test_parent_and_running_worker_argv_is_exact(self):
        stub = Stub()
        octet = os.path.join(stub.directory.name, "octet")
        try:
            with stub.path(TMUX=None):
                plan = plan_open_all(
                    multiplexer="tmux",
                    parent_session_id=PARENT_ID,
                    workers=[worker(name="explore-auth", index=1)],
                    workspace="/workspace",
                )
        finally:
            stub.close()
        self.assertIsInstance(plan, LaunchPlan)
        self.assertEqual(plan.session_name, "octet-fleet-%s" % PARENT_ID[:32])
        self.assertEqual(
            plan.panes[0].argv,
            (
                "tmux",
                "new-session",
                "-d",
                "-s",
                "octet-fleet-%s" % PARENT_ID[:32],
                "-n",
                "parent",
                "-c",
                "/workspace",
                "--",
                octet,
                "--resume",
                PARENT_ID,
            ),
        )
        self.assertEqual(
            plan.panes[1].argv,
            (
                "tmux",
                "new-window",
                "-d",
                "-t",
                "octet-fleet-%s" % PARENT_ID[:32],
                "-n",
                "explore-auth",
                "-c",
                "/workspace",
                "--",
                octet,
                "--resume",
                fake_session_reference("agent-1"),
            ),
        )
        # Every element is a separate argv element: no interpolation anywhere.
        for pane in plan.panes:
            self.assertIsInstance(pane.argv, tuple)
            self.assertTrue(all(isinstance(token, str) and token for token in pane.argv))
            self.assertNotIn(" ", pane.handle)

    def test_inside_tmux_reuses_the_current_session(self):
        stub = Stub()
        try:
            with stub.path(TMUX="/tmp/tmux-501/default,123,0"):
                plan = plan_open_all(
                    multiplexer="tmux",
                    parent_session_id=PARENT_ID,
                    workers=[worker()],
                    workspace=None,
                )
        finally:
            stub.close()
        self.assertTrue(plan.inside_multiplexer)
        self.assertEqual(plan.panes[0].argv[:3], ("tmux", "new-window", "-d"))
        self.assertEqual(plan.panes[1].argv[:3], ("tmux", "new-window", "-d"))
        self.assertNotIn("new-session", plan.panes[0].argv)
        self.assertIsNone(plan.session_name)

    def test_only_running_workers_get_a_pane(self):
        stub = Stub()
        try:
            with stub.path(TMUX=None):
                plan = plan_open_all(
                    multiplexer="tmux",
                    parent_session_id=PARENT_ID,
                    workers=[
                        worker("running", name="live", index=1),
                        worker("done", name="finished", index=2),
                        worker("failed", name="broken", index=3),
                        worker("orphaned", name="detached", index=4),
                        worker("stopped", name="halted", index=5),
                        worker("queued", name="pending", index=6),
                    ],
                    workspace=None,
                )
        finally:
            stub.close()
        self.assertEqual([pane.name for pane in plan.panes], ["parent", "live", "pending"])
        # A worker pane is planned and validated, but its only host handle is the
        # path-free opaque reference, which `octet --resume` cannot resolve today.
        self.assertEqual([pane.resolvable for pane in plan.panes], [True, False, False])
        self.assertEqual(plan.blocked[1].blocked_reason, plan.panes[1].blocked_reason)
        self.assertIn("agent-session", plan.panes[1].blocked_reason)

    def test_herdr_requires_ownership_and_builds_one_command_string(self):
        stub = Stub()
        try:
            with stub.path(HERDR_ENV=None):
                with self.assertRaises(Exception) as raised:
                    plan_open_all(
                        multiplexer="herdr",
                        parent_session_id=PARENT_ID,
                        workers=[worker()],
                        workspace=None,
                    )
                self.assertEqual(
                    getattr(raised.exception, "code", None), "multiplexer_not_owner"
                )
            with stub.path(HERDR_ENV="1"):
                plan = plan_open_all(
                    multiplexer="herdr",
                    parent_session_id=PARENT_ID,
                    workers=[worker()],
                    workspace=None,
                )
                outcome = execute_plan(plan)
                calls = stub.recorded()
        finally:
            stub.close()
        self.assertEqual(plan.panes[0].argv[0], "__herdr__")
        self.assertEqual(
            plan.panes[0].argv[1:],
            (os.path.join(stub.directory.name, "octet"), "--resume", PARENT_ID),
        )
        # The pane split and the submitted command are two separate argv calls.
        self.assertEqual(
            calls,
            [
                ["pane", "split", "--current", "--direction", "down", "--no-focus"],
                [
                    "pane",
                    "run",
                    "%7",
                    "%s --resume %s"
                    % (os.path.join(stub.directory.name, "octet"), PARENT_ID),
                ],
            ],
        )
        self.assertEqual(len(outcome.created), 1)

    def test_no_credentials_or_tokens_reach_a_command_or_a_notice(self):
        stub = Stub()
        try:
            with stub.path(TMUX=None):
                plan = plan_open_all(
                    multiplexer="tmux",
                    parent_session_id=PARENT_ID,
                    workers=[worker()],
                    workspace="/workspace",
                )
                outcome = execute_plan(plan)
                text = render_outcome(plan, outcome)
        finally:
            stub.close()
        blob = " ".join(token for row in plan_argv_rows(plan) for token in row["argv"])
        for secret in tuple(SECRETS.values()) + ("API_KEY", "TOKEN", "token="):
            self.assertNotIn(secret, blob)
            self.assertNotIn(secret, text)


class ExecutionTests(unittest.TestCase):
    def test_execute_plan_reports_partial_failure_and_stays_re_runnable(self):
        stub = Stub()
        try:
            with stub.path(TMUX=None):
                with mock.patch.dict(os.environ, {"STUB_FAIL_ON": "1"}):
                    plan = plan_open_all(
                        multiplexer="tmux",
                        parent_session_id=PARENT_ID,
                        workers=[],
                        workspace="/workspace",
                    )
                    outcome = execute_plan(plan, workspace="/workspace")
                text = render_outcome(plan, outcome)
                calls = stub.recorded()
        finally:
            stub.close()
        self.assertFalse(outcome.ok)
        self.assertEqual(len(outcome.created), 0)
        self.assertEqual(outcome.failure["returncode"], 1)
        self.assertIn("open-all stopped at pane parent/parent", text)
        self.assertIn("re-run", text)
        self.assertEqual(
            calls,
            [
                [
                    "new-session",
                    "-d",
                    "-s",
                    "octet-fleet-parent-session",
                    "-n",
                    "parent",
                    "-c",
                    "/workspace",
                    "--",
                    os.path.join(stub.directory.name, "octet"),
                    "--resume",
                    PARENT_ID,
                ]
            ],
        )

    def test_a_failed_pane_is_reported_exactly_and_the_command_stays_re_runnable(self):
        """Clean failure: report exactly what exists, destroy nothing else, re-run."""
        stub = Stub()
        try:
            with stub.path(TMUX=None):
                with mock.patch.dict(os.environ, {"STUB_FAIL_ON": "1"}):
                    plan = plan_open_all(
                        multiplexer="tmux",
                        parent_session_id=PARENT_ID,
                        workers=[worker(name="explore-auth", index=1)],
                        workspace="/workspace",
                    )
                    first = execute_plan(plan, workspace="/workspace")
                    first_calls = stub.recorded()
                    # Re-runnable: the identical request is attempted again.
                    second = execute_plan(plan, workspace="/workspace")
                    second_calls = stub.recorded()
        finally:
            stub.close()
        self.assertEqual(len(first.created), 0)
        self.assertEqual(first.failure["pane"]["role"], "parent")
        self.assertEqual(first.failure["returncode"], 1)
        self.assertIn("stub failure", first.failure["stderr"])
        self.assertEqual(len(first_calls), 1, "one failed pane stops the run")
        self.assertEqual(first_calls[0][0], "new-session")
        self.assertEqual(
            second_calls[:1], first_calls, "re-running issues the same first command"
        )
        self.assertEqual(len(second_calls), 2)
        self.assertEqual(len(second.blocked), 1, "the worker pane is still blocked")
        # Transient failure cleared: the identical re-run opens the pane and the
        # blocked worker pane is reported, not silently dropped.
        self.assertTrue(second.ok)
        self.assertEqual(len(second.created), 1)

    def test_open_all_report_names_running_workers_and_the_unchanged_panel(self):
        stub = Stub()
        try:
            with stub.path(TMUX=None):
                orchestrator = Orchestrator(publish=lambda snapshot: None)
                result = orchestrator.open_all(
                    owner=owner(),
                    workers=[worker(name="explore-auth", index=1), worker("done", name="finished", index=2)],
                    arguments=["tmux"],
                )
        finally:
            stub.close()
        self.assertIn("open-all tmux", result["text"])
        self.assertIn("unchanged", result["text"])
        self.assertEqual([row["role"] for row in result["panes"]], ["parent", "worker"])
        self.assertEqual([row["name"] for row in result["panes"]], ["parent", "explore-auth"])
        self.assertEqual(len(result["notifications"]), 1)
        self.assertIn("missing a host primitive", result["notifications"][0]["title"])

    def test_open_all_requires_a_multiplexer_argument(self):
        stub = Stub()
        try:
            with stub.path(TMUX=None):
                orchestrator = Orchestrator(publish=lambda snapshot: None)
                with self.assertRaises(Exception) as raised:
                    orchestrator.open_all(owner=owner(), workers=[worker()], arguments=[])
        finally:
            stub.close()
        self.assertEqual(getattr(raised.exception, "code", None), "unsupported_multiplexer")


@unittest.skipUnless(shutil.which("tmux"), "tmux is not installed on this host")
class RealTmuxTests(unittest.TestCase):
    def test_real_tmux_creates_a_detached_session_then_reaps_it(self):
        """Guarded on availability; the created session is destroyed in teardown."""
        stub = Stub()
        session = "octet-openall-real-%d" % os.getpid()
        try:
            with stub.path(TMUX=None):
                plan = plan_open_all(
                    multiplexer="tmux",
                    parent_session_id=PARENT_ID,
                    workers=[],
                    workspace=None,
                )
            parent = plan.panes[0]
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
