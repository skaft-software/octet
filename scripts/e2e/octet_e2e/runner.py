"""Check runner: selection, isolation, reporting, exit status."""

from __future__ import annotations

import argparse
import json
import shutil
import sys
import tempfile
import traceback
from dataclasses import dataclass, field
from pathlib import Path
from typing import Callable

from .fixtures import Environment, build_environment, octet_args
from .provider import MockProvider
from .session import CheckFailure, OctetSession


class CheckSkip(RuntimeError):
    """The check cannot run on this host; not a product failure."""


@dataclass
class CheckResult:
    name: str
    status: str  # pass | fail | skip
    detail: str = ""
    evidence: list[str] = field(default_factory=list)


@dataclass
class CheckSpec:
    name: str
    run: Callable[["Context"], list[str]]
    description: str


class Context:
    """Shared state for one suite run."""

    def __init__(
        self,
        *,
        binary: Path,
        repo_root: Path,
        scratch_root: Path,
        matrix_root: Path,
        keep: bool = False,
        verbose: bool = False,
        timeout_scale: float = 1.0,
    ):
        self.binary = Path(binary)
        self.repo_root = Path(repo_root)
        self.scratch_root = Path(scratch_root)
        self.matrix_root = Path(matrix_root)
        self.keep = keep
        self.verbose = verbose
        self.timeout_scale = timeout_scale
        self.env_counter = 0
        self.session_counter = 0
        self._providers: list[MockProvider] = []
        self._sessions: list[OctetSession] = []
        self.scratch_root.mkdir(parents=True, exist_ok=True)
        (self.scratch_root / "logs").mkdir(exist_ok=True)

    # ----------------------------------------------------------- providers
    def provider(self) -> MockProvider:
        provider = MockProvider()
        self._providers.append(provider)
        return provider

    def environment(self, name: str, provider: MockProvider) -> Environment:
        self.env_counter += 1
        return build_environment(self.scratch_root, f"{self.env_counter:02d}-{name}", provider.port)

    # ------------------------------------------------------------ sessions
    def start(
        self,
        env: Environment,
        *,
        name: str,
        args_extra: list[str] | None = None,
        columns: int = 120,
        rows: int = 40,
        wait_ready: bool = True,
        child_env_extra: dict[str, str] | None = None,
        mouse: str = "app",
        theme: str | None = None,
        extension_dirs: list[Path] | None = None,
        enable_extensions: list[str] | None = None,
        agent: bool = True,
        set_name: bool = True,
    ) -> OctetSession:
        self.session_counter += 1
        log = self.scratch_root / "logs" / f"{self.session_counter:02d}-{name}.ansi"
        args = octet_args(
            env,
            name=name if set_name else None,
            mouse=mouse,
            theme=theme,
            extension_dirs=extension_dirs,
            enable_extensions=enable_extensions,
            agent=agent,
            columns=columns,
            rows=rows,
            extra=args_extra,
        )
        session = OctetSession(
            self.binary,
            args=args,
            env=env.child_env(self.repo_root, extra=child_env_extra),
            cwd=env.workspace,
            columns=columns,
            rows=rows,
            log_path=log,
        )
        self._sessions.append(session)
        if wait_ready:
            session.wait_ready(env.model_label, timeout=30 * self.timeout_scale)
        return session

    def cleanup(self) -> None:
        for session in self._sessions:
            if session.exit_code is None:
                session.kill()
        for provider in self._providers:
            provider.close()
        if not self.keep:
            shutil.rmtree(self.scratch_root, ignore_errors=True)


def run_checks(specs: list[CheckSpec], ctx: Context) -> tuple[list[CheckResult], int]:
    results: list[CheckResult] = []
    for spec in specs:
        try:
            evidence = spec.run(ctx) or []
            results.append(CheckResult(spec.name, "pass", "", evidence))
            print(f"PASS {spec.name}" + (f" — {evidence[-1]}" if evidence else ""))
        except CheckSkip as skip:
            results.append(CheckResult(spec.name, "skip", str(skip)))
            print(f"SKIP {spec.name} — {skip}")
        except CheckFailure as failure:
            results.append(CheckResult(spec.name, "fail", str(failure)))
            print(f"FAIL {spec.name} — {first_line(str(failure))}")
        except Exception:  # noqa: BLE001 - any harness error is a failed check
            detail = traceback.format_exc()
            results.append(CheckResult(spec.name, "fail", detail))
            print(f"FAIL {spec.name} — harness error:\n{detail}")
        if ctx.verbose:
            for line in results[-1].evidence:
                print(f"    {line}")
        sys.stdout.flush()
    return results, 1 if any(result.status == "fail" for result in results) else 0


def first_line(text: str) -> str:
    lines = [line for line in text.splitlines() if line.strip()]
    return lines[0] if lines else text


def summarize(results: list[CheckResult], *, report_path: Path | None = None) -> None:
    passed = sum(1 for result in results if result.status == "pass")
    failed = sum(1 for result in results if result.status == "fail")
    skipped = sum(1 for result in results if result.status == "skip")
    print(f"\n{passed} passed, {failed} failed, {skipped} skipped")
    if failed:
        print("failed checks: " + ", ".join(r.name for r in results if r.status == "fail"))
    if report_path is not None:
        report_path.write_text(
            json.dumps(
                [
                    {"name": r.name, "status": r.status, "detail": r.detail, "evidence": r.evidence}
                    for r in results
                ],
                indent=2,
            )
        )


def parse_args(argv: list[str]) -> argparse.Namespace:
    parser = argparse.ArgumentParser(
        prog="octet-e2e",
        description="End-to-end regression suite for a published octet binary.",
    )
    default_binary = Path.home() / "src" / "octet-release" / "bin" / "octet-latest"
    default_matrix = Path.home() / "src" / "octet-release" / "matrix"
    parser.add_argument("--binary", type=Path, default=default_binary, help="octet binary under test")
    parser.add_argument("--matrix", type=Path, default=default_matrix, help="Pi extension matrix root (ORDERS/matrix)")
    parser.add_argument(
        "--checks",
        default="all",
        help="comma-separated check names or globs (default: all); 'core'/'compat' select groups",
    )
    parser.add_argument("--list", action="store_true", help="list check names and exit")
    parser.add_argument("--keep", action="store_true", help="keep the scratch directory for inspection")
    parser.add_argument("--scratch", type=Path, default=None, help="scratch directory (default: a fresh temp dir)")
    parser.add_argument("--verbose", action="store_true", help="print every evidence line")
    parser.add_argument("--timeout-scale", type=float, default=1.0, help="scale all waits (slow machines)")
    parser.add_argument("--report", type=Path, default=None, help="write a JSON report to this path")
    parser.add_argument("--self-test", action="store_true", help="run the suite's own unit checks and exit")
    return parser.parse_args(argv)


def main(argv: list[str] | None = None) -> int:
    args = parse_args(list(sys.argv[1:] if argv is None else argv))
    from .checks import CHECKS
    from .checks_pi import PI_CHECKS
    from .selftest import selftest

    if args.self_test:
        return 0 if selftest() else 1

    all_checks = CHECKS + PI_CHECKS
    if args.list:
        for spec in all_checks:
            print(f"{spec.name}\t{spec.description}")
        return 0

    selected = select_checks(all_checks, args.checks)
    if not selected:
        print(f"no checks match {args.checks!r}", file=sys.stderr)
        return 2

    binary = args.binary.expanduser().resolve()
    if not binary.exists():
        print(f"binary not found: {binary}", file=sys.stderr)
        return 2
    repo_root = Path(__file__).resolve().parents[3]
    scratch = args.scratch or Path(tempfile.mkdtemp(prefix="octet-e2e-"))
    print(f"octet E2E suite\n  binary:  {binary}\n  scratch: {scratch}\n  checks:  {', '.join(s.name for s in selected)}\n")

    ctx = Context(
        binary=binary,
        repo_root=repo_root,
        scratch_root=scratch,
        matrix_root=args.matrix.expanduser(),
        keep=args.keep,
        verbose=args.verbose,
        timeout_scale=args.timeout_scale,
    )
    try:
        results, status = run_checks(selected, ctx)
    finally:
        ctx.cleanup()
    summarize(results, report_path=args.report)
    return status


def select_checks(all_checks: list[CheckSpec], expression: str) -> list[CheckSpec]:
    expression = expression.strip()
    if expression in ("all", ""):
        return all_checks
    if expression == "core":
        from .checks import CHECKS

        return list(CHECKS)
    if expression == "compat":
        from .checks_pi import PI_CHECKS

        return list(PI_CHECKS)
    wanted: list[str] = []
    for token in expression.split(","):
        token = token.strip()
        if token:
            wanted.append(token)
    selected = []
    for spec in all_checks:
        for token in wanted:
            if token == spec.name or (token.endswith("*") and spec.name.startswith(token[:-1])):
                selected.append(spec)
                break
    return selected
