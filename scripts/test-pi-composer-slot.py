#!/usr/bin/env python3
"""Offline real-binary composer-slot check with unchanged matrix Pi extensions.

Preserves real HOME/config; isolates workspace, sessions and extension discovery
with CLI flags. Never submits provider input. All signalled PIDs are our own.
"""
import argparse
import importlib.util
import json
import os
from pathlib import Path
import shutil
import subprocess
import tempfile

ROOT = Path(__file__).resolve().parents[1]
spec = importlib.util.spec_from_file_location("pi_pty", ROOT / "scripts/test-pi-compat-pty.py")
pty = importlib.util.module_from_spec(spec)
spec.loader.exec_module(pty)


class Screen(pty.Screen):
    def csi(self, final):
        if final == "u" and self.sequence.startswith((">", "<", "=", "?")):
            return
        super().csi(final)


pty.Screen = Screen


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("binary", type=Path)
    parser.add_argument("--footer", type=Path, required=True)
    parser.add_argument("--editor", type=Path, required=True)
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--model", default="codex/gpt-6-luna")
    args = parser.parse_args()
    args.output.mkdir(parents=True, exist_ok=True, mode=0o700)
    args.output.chmod(0o700)
    adapter = ROOT / "extensions/octet-pi-compat"
    entries = [args.footer.resolve(), args.editor.resolve()]
    report = {"binary_sha256": pty.sha256(args.binary),
              "extensions": [{"path": str(p), "sha256": pty.sha256(p)} for p in entries]}
    with tempfile.TemporaryDirectory(prefix="composer-slot-", dir=args.output) as scratch:
        root = Path(scratch)
        workspace, sessions, extensions = root / "workspace", root / "sessions", root / "extensions"
        workspace.mkdir(); sessions.mkdir()
        subprocess.run([shutil.which("node"), str(adapter / "configure.mjs"), "--reviewed",
                        "--output", str(extensions / "octet-pi-compat"), *map(str, entries)],
                       check=True, timeout=30, capture_output=True)
        environment = dict(os.environ, TERM="xterm-256color", COLORTERM="truecolor", OCTET_TERN="off")
        command = [str(args.binary.resolve()), "--offline", "--model", args.model,
                   "--workspace", str(workspace), "--session-dir", str(sessions),
                   "--extension-dir", str(extensions), "--enable-extension", "octet-pi-compat",
                   "--trust-extension", "octet-pi-compat", "--effect-policy", "unsafe_host",
                   "--allow-shell", "--no-context-files", "--no-tools"]
        terminal = pty.TerminalProcess(command, environment, workspace)
        try:
            terminal.until(lambda s: "Working" not in s.text() and "─" in s.text(), label="ready composer")
            terminal.pump(1)
            terminal.send(b"u17-slot-PTY")
            terminal.until(lambda s: s.text().count("u17-slot-PTY") == 1, label="one editor draft")
            draft = terminal.screen.text()
            terminal.send(b"\x07")  # Ctrl+G must not retire the composer.
            terminal.pump(0.5)
            assert terminal.screen.text().count("u17-slot-PTY") == 1, terminal.screen.text()
            terminal.send(b"\x03")  # Native clear, not submission.
            terminal.until(lambda s: "u17-slot-PTY" not in s.text(), label="native clear")
            terminal.send(b"/")
            terminal.until(lambda s: "/model" in s.text() and "navigate" in s.text(), label="native slash popup")
            popup = terminal.screen.text()
            assert popup.count("navigate") == 1, popup
            assert popup.count("/model") == 1, popup
            terminal.resize(110, 36)
            terminal.until(lambda s: "/model" in s.text(), label="resized native slash popup")
            terminal.close()
            report.update({"result": "pass", "draft_occurrences": draft.count("u17-slot-PTY"),
                           "slash_popups": popup.count("navigate"), "coordinated_close": True})
            for name, value in [("draft.txt", draft), ("slash.txt", popup)]:
                path = args.output / name
                path.write_text(value + "\n"); path.chmod(0o600)
        except Exception as error:
            report.update({"result": "fail", "error": str(error)})
            raise
        finally:
            terminal.dispose()
            # A failed real-binary probe needs the same private candidate and
            # byte evidence as a pass; do not lose it during fixture cleanup.
            path = args.output / "capture.ansi"
            path.write_bytes(terminal.capture); path.chmod(0o600)
            path = args.output / "result.json"
            path.write_text(json.dumps(report, indent=2) + "\n"); path.chmod(0o600)
    print(json.dumps(report))


if __name__ == "__main__":
    main()
