#!/usr/bin/env python3
"""Offline smoke test: Pi 1.5.2 and the unmodified pi-claude-code package."""
import argparse
import json
import os
from pathlib import Path
import shutil
import subprocess
from types import SimpleNamespace

from drive import Term

HERE = Path(__file__).resolve().parent
REVISION = "d5b5e04c314d3452d357174d16d10dda34daa2d2"


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--octet", type=Path, required=True)
    parser.add_argument("--pi-install", type=Path, required=True)
    parser.add_argument("--package", type=Path, required=True,
                        help=f"unmodified pi-claude-code checkout or archive of {REVISION}")
    parser.add_argument("--out", type=Path, required=True, help="new directory outside the repository")
    args = parser.parse_args()
    octet, pi, package, root = (path.resolve() for path in
                                (args.octet, args.pi_install, args.package, args.out))
    metadata = json.loads((pi / "node_modules/@earendil-works/pi-coding-agent/package.json").read_text())
    assert metadata["version"] == "1.5.2", metadata["version"]
    if (package / ".git").exists():
        assert subprocess.check_output(["git", "-C", package, "rev-parse", "HEAD"], text=True).strip() == REVISION
        assert not subprocess.check_output(["git", "-C", package, "status", "--porcelain", "--untracked-files=no"])

    root.mkdir(mode=0o700)
    home, ws, tmp = (root / name for name in ("home", "workspace", "tmp"))
    agent = home / ".pi/agent"
    install = agent / "install"
    for folder in (ws, tmp, install / "releases"):
        folder.mkdir(parents=True)
    (install / "releases/1.5.2").symlink_to(pi)
    (install / "current-version").write_text("1.5.2\n")
    settings = agent / "settings.json"
    settings.write_text(json.dumps({"packages": [str(package)]}))
    original_settings = settings.read_bytes()
    (ws / "read-me.txt").write_text("FILE-READ-ROUNDTRIP-17\n")
    fake = root / "fake-claude-code.mjs"
    shutil.copyfile(HERE / "fixtures/fake-claude-code.mjs", fake)
    fake.chmod(0o700)
    node = Path(shutil.which("node")).parent
    env = {
        "PATH": f"{node}:/usr/bin:/bin", "HOME": str(home), "TMPDIR": str(tmp),
        "LANG": "C.UTF-8", "TERM": "xterm-256color", "OCTET_COLOR_SCHEME": "dark",
        "OCTET_TERN": "off", "PI_CLAUDE_CODE_COMMAND": str(fake),
    }
    extensions = root / "extensions"
    shutil.copytree(HERE.parents[1] / "extensions/octet-pi-compat", extensions / "octet-pi-compat",
                    symlinks=True, ignore=shutil.ignore_patterns("test"))
    command = [str(octet), "--offline", "--workspace", str(ws), "--enable-extension", "octet-pi-compat",
               "--extension-dir", str(extensions),
               "--no-context-files", "--model", "claude-code/claude-sonnet-5"]
    invocations = home / "fake-claude.jsonl"
    (home / "fixture-mode").write_text("signed\n")
    term = Term(SimpleNamespace(root=str(root), ws=str(ws), exe=str(octet), env=env,
                                args=lambda *extra: command + list(extra)))
    try:
        term.pump(3)
        term.send(b"/claude-code-status\r")
        term.pump(3)
    finally:
        term.close()
    assert b"Claude Code is installed and logged in" in (root / "raw.bin").read_bytes()
    status = [json.loads(line) for line in invocations.read_text().splitlines()]
    assert status and all(call["kind"] == "status" for call in status), status
    print("PASS: status uses CLI auth status without inference")
    invocations.unlink()

    for mode, expected in [("text", "FIXTURE-PLAIN-TEXT"), ("tool", "FIXTURE-TOOL-FOLLOWUP"),
                           ("signed", "FIXTURE-TOOL-FOLLOWUP")]:
        (home / "fixture-mode").write_text(mode + "\n")
        if invocations.exists():
            invocations.unlink()
        out = subprocess.run(command + ["--thinking", "high" if mode == "signed" else "off",
                                        "--max-turns", "3", "-p", "Local fixture prompt."],
                             cwd=ws, env=env, capture_output=True, text=True, timeout=30)
        (root / f"{mode}.stdout").write_text(out.stdout)
        (root / f"{mode}.stderr").write_text(out.stderr)
        assert out.returncode == 0 and expected in out.stdout, (mode, out.returncode, out.stderr)
        calls = [json.loads(line) for line in invocations.read_text().splitlines()]
        (root / f"{mode}.jsonl").write_bytes(invocations.read_bytes())
        if mode != "text":
            parts = [part for call in calls if call["kind"] == "input"
                     for part in call["frame"].get("message", {}).get("content", [])]
            assert any(part["type"] == "tool_result" and part["tool_use_id"] == "tool_fixture_17"
                       and "FILE-READ-ROUNDTRIP-17" in part["content"] and not part["is_error"] for part in parts)
        print(f"PASS: {mode}")

    sessions = b"".join(path.read_bytes() for path in (home / ".octet/sessions").rglob("*.jsonl"))
    assert b"VISIBLE-THINKING-PROBE" in sessions
    assert b"opaque-signature-do-not-export" not in sessions
    assert b"opaque-redacted-do-not-export" not in sessions
    assert settings.read_bytes() == original_settings and not (agent / "auth.json").exists()
    print("PASS: visible reasoning retained without opaque signatures; Pi settings unchanged")


if __name__ == "__main__":
    main()
