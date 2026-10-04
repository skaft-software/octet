#!/usr/bin/env python3
"""Strict offline original-editor PTY probe; no inference or HOME discovery.

Uses the common PTY observer but waits for removal of the *foreign* editor
before checking the recovered native draft. An old painted snapshot is not a
handoff acknowledgement. Supply every original entrypoint explicitly.
"""
import argparse
import hashlib
import importlib.util
import json
from pathlib import Path
import shutil
import tempfile

ROOT = Path(__file__).resolve().parents[3]
spec = importlib.util.spec_from_file_location("pi_pty", ROOT / "scripts/test-pi-compat-pty.py")
pty = importlib.util.module_from_spec(spec)
spec.loader.exec_module(pty)


class Screen(pty.Screen):
    def csi(self, final):
        # Kitty keyboard push/pop/query use private CSI u prefixes. They do
        # not restore the cursor (plain CSI u), including on editor rescue.
        if final == "u" and self.sequence.startswith((">", "<", "=", "?")):
            return
        super().csi(final)


pty.Screen = Screen


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("binary", type=Path)
    parser.add_argument("--editor", type=Path, required=True)
    parser.add_argument("--adapter", type=Path, default=Path(__file__).resolve().parents[1])
    parser.add_argument("--node", type=Path, default=Path(shutil.which("node") or "/missing-node"))
    parser.add_argument("--capture", type=Path)
    parser.add_argument("--burst", action="store_true",
                        help="rescue as soon as a complete burst-input frame appears; no checkpoint grace period")
    args = parser.parse_args()
    for key in ("binary", "editor", "adapter", "node"):
        setattr(args, key, getattr(args, key).resolve(strict=True))
    with tempfile.TemporaryDirectory(prefix="octet-editor-native-") as name:
        root = Path(name).resolve()
        environment, workspace = pty.isolated(root)
        extensions = pty.configure(args.adapter, args.node, root, [args.editor], environment, workspace)
        terminal = pty.TerminalProcess(pty.native_command(args.binary, extensions), environment, workspace)
        try:
            terminal.until(lambda s: "probe" in s.text() and "Ctrl+G restore editor" in s.text(), label="foreign editor mounted")
            text = "ultrathink"
            if args.burst:
                terminal.send(text.encode())
            else:
                for char in text:
                    terminal.send(char.encode())
                    terminal.pump(0.012)
            terminal.until(lambda s: text in s.text(), label="complete foreign draft")
            if not args.burst:
                terminal.pump(0.5)  # grace period, not a protocol checkpoint acknowledgement
                terminal.resize(72, 24)
                terminal.pump(0.3)
            frame = terminal.screen.frames
            terminal.send(b"\x07")
            terminal.until(lambda s: s.frames > frame and "Ctrl+G restore editor" not in s.text(), label="post-rescue native frame")
            if text not in terminal.screen.text():
                raise AssertionError("native rescue lost the complete draft\n" + terminal.screen.text())
            terminal.send(b"-native")
            terminal.until(lambda s: text + "-native" in s.text(), label="recovered native draft remains editable")
            # Late composer writes can mutate host text without repainting it.
            # Force a later native edit; do not accept a stale post-rescue frame.
            terminal.pump(0.5)
            terminal.send(b"!")
            terminal.until(lambda s: text + "-native!" in s.text(), label="native draft survives late editor writes")
            if args.burst:
                terminal.resize(72, 24)
                terminal.pump(0.3)
            terminal.send(b"\x03")
            terminal.pump(0.1)
            terminal.close()
            print(json.dumps({"editor": "pass", "entrypoint": str(args.editor),
                "sha256": hashlib.sha256(args.editor.read_bytes()).hexdigest(),
                "rescue": "post-rescue native frame + complete editable draft + later edit",
                "input": "burst without grace period" if args.burst else "paced with grace period", "resize": "pass",
                "shutdown": "pass", "first_frame_ms": terminal.first_frame_ms,
                "captured_bytes": len(terminal.capture)}, indent=2))
        finally:
            if args.capture:
                args.capture.write_bytes(terminal.capture)
                args.capture.chmod(0o600)
            terminal.dispose()


if __name__ == "__main__":
    main()
