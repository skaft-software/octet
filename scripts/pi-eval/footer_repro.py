#!/usr/bin/env python3
"""Record a footer/startup window of octet or Pi on a built fixture.

Usage: footer_repro.py {octet|pi} {workspace|home} OUTDIR --fixture ROOT [--seconds 20] [--prompt TEXT]

Starts the product on a PTY with the model `gpt-5.6-luna` (display name `GPT-5.6 Luna`, 272K context),
records timestamped PTY chunks for the window (analyse them with `flicker.py report`), optionally sends one
prompt, and writes the final screen. Run it under `flock -s <measure.lock>`.
"""

from __future__ import annotations

import argparse
import json
import os
import shutil
import subprocess
import sys
import time
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parents[1] / "e2e"))
sys.path.insert(0, str(Path(__file__).resolve().parent))
import core  # noqa: E402
import fixture as fx  # noqa: E402
import run  # noqa: E402
import flicker  # noqa: E402
from octet_e2e.provider import MockProvider  # noqa: E402

REPO = Path(__file__).resolve().parents[2]
MODEL_ID, MODEL_NAME, CONTEXT = "gpt-5.6-luna", "GPT-5.6 Luna", 272000


def prepare_linked_home(fixture: fx.Fixture, env) -> None:
    """Fresh home whose read-only `install/` and `npm/` trees are hardlinks (disk is shared); the rest is copied."""
    if env.root.exists():
        shutil.rmtree(env.root)
    env.home.mkdir(parents=True)
    source = fixture.agent
    target = env.home / ".pi" / "agent"
    target.mkdir(parents=True)
    for entry in source.iterdir():
        if entry.name in ("install", "npm"):
            subprocess.run(["cp", "-al", str(entry), str(target / entry.name)], check=True)
        else:
            fx.copy_tree(entry, target / entry.name)
    for entry in fixture.home.iterdir():
        if entry.name != ".pi":
            fx.copy_tree(entry, env.home / entry.name)


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("target", choices=["octet", "pi"])
    parser.add_argument("cwd", choices=["workspace", "home"])
    parser.add_argument("out", type=Path)
    parser.add_argument("--fixture", type=Path, required=True, help="fixture root (contains home/ and fixture.json)")
    parser.add_argument("--octet", type=Path, default=REPO / "target" / "release" / "octet")
    parser.add_argument("--extensions-dir", type=Path, default=REPO / "extensions")
    parser.add_argument("--seconds", type=float, default=20.0)
    parser.add_argument("--prompt")
    parser.add_argument("--warm", action="store_true", help="launch once first so the recorded launch starts warm")
    parser.add_argument("--homes", type=Path, help="persistent homes directory; an existing home is reused (warm caches) instead of copied")
    args = parser.parse_args(argv)

    out = args.out
    shutil.rmtree(out, ignore_errors=True)
    out.mkdir(parents=True)
    meta = json.loads((args.fixture / "fixture.json").read_text())
    fixture = fx.Fixture(root=args.fixture, lock={"pi": {"version": meta["pi_version"]}})
    provider = MockProvider(model_id=MODEL_ID)
    provider.default_reply("FOOTER-REPLY-OK")
    env = run.make_env(args.homes or out / "homes", args.target, 0, provider.port)
    prepare_linked_home(fixture, env)
    fx.write_provider_config(env.home, provider.port, model=MODEL_ID, name=MODEL_NAME, context_window=CONTEXT)
    tmp = run.leg_tmp(env)
    tmp.mkdir(parents=True, exist_ok=True)
    env.workspace.mkdir(parents=True, exist_ok=True)
    env.sessions.mkdir(parents=True, exist_ok=True)
    cwd = env.home if args.cwd == "home" else env.workspace
    if args.target == "octet" and args.homes is None:
        run.reset_adapter_cache(args.extensions_dir)
        launch_args = run.octet_args(
            env, name=None, agent=False, extension_dirs=[args.extensions_dir], enable_extensions=["octet-pi-compat"],
        )
        launch_args[launch_args.index("--model") + 1] = f"custom/{MODEL_ID}"
        launch_args[launch_args.index("--workspace") + 1] = str(cwd)
        binary = args.octet
        child = env.child_env(REPO, extra={"OCTET_STARTUP_TRACE": "1", "TMPDIR": str(tmp)})
    else:
        binary = Path("node")
        launch_args = [str(fixture.pi_cli), "--model", f"{fx.PROVIDER}/{MODEL_ID}"]
        child = {k: v for k, v in os.environ.items() if not k.startswith(("PI_", "OCTET_"))}
        child.update(HOME=str(env.home), TERM="xterm-256color", COLORTERM="truecolor", LANG="C.UTF-8", LC_ALL="C.UTF-8",
                     PI_OFFLINE="1", PI_SKIP_VERSION_CHECK="1", PI_TELEMETRY="0", TMPDIR=str(tmp))

    def start(log: str) -> run.EvalSession:
        return run.EvalSession(binary, args=launch_args, env=child, cwd=cwd, columns=120, rows=40,
                               log_path=out / f"{log}.ansi", stderr_path=out / f"{log}.stderr.log")

    if args.warm:
        warm = start("warmup")
        deadline = time.monotonic() + 90
        while core.READY_MARKER not in warm.screen_text() and time.monotonic() < deadline:
            time.sleep(0.1)
        warm.close(timeout=10)
    session = start("transcript")
    started = time.monotonic()
    ready, sent = None, False
    while time.monotonic() - started < args.seconds:
        if ready is None and core.READY_MARKER in session.screen_text():
            ready = time.monotonic() - started
        if args.prompt and not sent and ready is not None and time.monotonic() - started > ready + 1:
            session.submit(args.prompt)
            sent = True
        time.sleep(0.05)
    (out / "screen.txt").write_text(session.screen_text())
    flicker.dump_chunks([(t - session.started_at, d) for t, d in session.chunks], out / "chunks.jsonl")
    code = session.close(timeout=10)
    provider.close()
    if args.homes is None:
        shutil.rmtree(env.root, ignore_errors=True)
    print(f"{args.target} {args.cwd}: ready={ready} exit={code} chunks={len(session.chunks)}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
