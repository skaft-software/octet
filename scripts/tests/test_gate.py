"""Keep the local main gate aligned with Linux quality/test commands."""

import os
from pathlib import Path
import re
import shlex
import subprocess
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[2]
GATE = ROOT / "scripts/gate.sh"
WORKFLOW = ROOT / ".github/workflows/ci.yml"


def jobs(workflow):
    return dict(re.findall(r"(?ms)^  ([\w-]+):\n(.*?)(?=^  [\w-]+:\n|\Z)",
                           workflow.split("\njobs:\n", 1)[1]))


def commands(text):
    return {tuple(shlex.split(match)) for match in re.findall(
        r"(?m)^\s*(?:- run: |run: |step \S+ )?((?:cargo |npm test |python3 )[^\n]+)$", text
    )}


class GateTests(unittest.TestCase):
    def shell(self, code, *args):
        return subprocess.run(
            ["bash", "-c", 'source scripts/gate.sh\n' + code, "gate-test", *args],
            cwd=ROOT, text=True, capture_output=True,
            env={**os.environ, "PYTHONDONTWRITEBYTECODE": "1"},
        )

    def test_linux_ci_commands_are_represented(self):
        workflow_jobs = jobs(WORKFLOW.read_text())
        gate = GATE.read_text()
        represented = commands(gate)
        # Named CI steps are read at runtime, including their dependency installs
        # and Python release tests. Check the real extractor, not a copied list.
        for job, name in re.findall(r"ci_(?:step|checks) (quality|test) '([^']+)'", gate):
            result = self.shell('ci_script "$1" "$2"', job, name)
            self.assertEqual(result.returncode, 0, result.stderr)
            self.assertIn(name, workflow_jobs[job])
            represented.update(commands(result.stdout))
        expected = commands(workflow_jobs["quality"]) | commands(workflow_jobs["test"])
        self.assertTrue(expected)
        for command in sorted(expected):
            with self.subTest(command=shlex.join(command)):
                self.assertIn(command, represented, "CI changed: update scripts/gate.sh")

    def test_step_continues_after_failure_and_counts_results(self):
        result = self.shell('passed=0; failed=0; step first false; step second true; '
                            'printf "counts=%s,%s\\n" "$passed" "$failed"')
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("FAIL first (exit 1)", result.stdout)
        self.assertIn("PASS second", result.stdout)
        self.assertIn("counts=1,1", result.stdout)

    def test_grouped_python_checks_continue_after_failure(self):
        result = self.shell('''
ci_script() { printf 'python3 -c "raise SystemExit(1)"\\npython3 -c "print(42)"\\n'; }
passed=0; failed=0
ci_checks quality grouped
printf 'counts=%s,%s\\n' "$passed" "$failed"
''')
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn('FAIL python3 -c "raise SystemExit(1)"', result.stdout)
        self.assertIn('PASS python3 -c "print(42)"', result.stdout)
        self.assertIn("counts=1,1", result.stdout)

    def test_missing_ci_step_fails_closed(self):
        result = self.shell("ci_script quality missing-step")
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("CI step missing", result.stderr)

    def test_help_and_bad_arguments(self):
        result = self.shell('main --help')
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("Only the full gate qualifies a push", result.stdout)
        for args in (("--base",), ("--base", "--quick"), ("--unknown",)):
            with self.subTest(args=args):
                result = self.shell('main "$@"', *args)
                self.assertEqual(result.returncode, 2, result.stderr)

    def test_quick_selects_touched_crates_and_keeps_running(self):
        with tempfile.TemporaryDirectory(prefix="octet-gate-test-") as directory:
            stub = Path(directory) / "stub"
            stub.write_text('''#!/usr/bin/env python3
import json
import os
from pathlib import Path
import sys
if Path(sys.argv[0]).name == "git":
    print(os.environ['GATE_TEST_FILES'], end='')
elif sys.argv[1] == 'metadata':
    print(json.dumps({'workspace_members': ['alpha', 'beta'], 'packages': [
        {'id': name, 'name': name, 'manifest_path': str(Path.cwd() / 'crates' / name / 'Cargo.toml')}
        for name in ('alpha', 'beta')]}))
else:
    print('TEST:' + ' '.join(sys.argv[1:]))
    sys.exit(1 if 'alpha' in sys.argv else 0)
''')
            stub.chmod(0o755)
            for command in ("cargo", "git"):
                (Path(directory) / command).symlink_to(stub)
            cases = (
                ("crates/beta/src/lib.rs", ("beta",), 0),
                ("Cargo.lock", ("alpha", "beta"), 1),
                ("docs/testing/README.md", (), 0),
            )
            for files, selected, status in cases:
                with self.subTest(files=files):
                    code = ('export PATH="$1:$PATH" GATE_TEST_FILES="$2"; '
                            'base=HEAD~1; quick_tests')
                    result = self.shell(code, directory, files)
                    self.assertEqual(result.returncode, status, result.stderr)
                    observed = re.findall(r'TEST:test -p (\S+)', result.stdout)
                    self.assertEqual(observed, list(selected), result.stdout)
                    for line in result.stdout.splitlines():
                        if line.startswith("TEST:"):
                            self.assertIn("--profile ci-test --locked --no-fail-fast", line)

    def test_modes_and_exit_status_without_running_builds(self):
        # Exercise main's control flow, without installing dependencies or compiling.
        mocks = '''
ci_step() { echo "CI:$*"; }
ci_checks() { echo "CI:$*"; }
pi_fixture() { echo fixture; }
npm() { echo "NPM:$*"; }
quick_tests() { echo "BASE:$base"; }
cargo() { echo "CARGO:$* TARGET:$CARGO_TARGET_DIR JOBS:$CARGO_BUILD_JOBS"; return 1; }
export CARGO_TARGET_DIR=relative-target CARGO_BUILD_JOBS=6
main "$@"
'''
        result = self.shell(mocks, "--quick", "--base", "HEAD~1")
        self.assertEqual(result.returncode, 1, result.stderr)
        self.assertIn("BASE:HEAD~1", result.stdout)
        self.assertIn("TARGET:relative-target JOBS:6", result.stdout)
        self.assertIn("GATE: 1 PASS, 2 FAIL", result.stdout)
        self.assertNotIn("CI:", result.stdout)
        self.assertNotIn("--workspace --all-targets --all-features --profile", result.stdout)
        result = self.shell(mocks)
        self.assertEqual(result.returncode, 1, result.stderr)
        self.assertIn("FAIL no-default", result.stdout)
        self.assertRegex(result.stdout, r"GATE: \d+ PASS, [1-9]\d* FAIL\n$")


if __name__ == "__main__":
    unittest.main()
