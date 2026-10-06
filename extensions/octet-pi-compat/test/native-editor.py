#!/usr/bin/env python3
"""Strict offline original-editor PTY probe; no inference or HOME discovery.

Uses host-acknowledged mount identities and matching host rescue notifications,
then newer terminal frames and native edits to check the recovered draft. Neither
an old painted snapshot nor an outgoing open request proves a handoff. Supply
every original entrypoint explicitly; instrumentation never changes the factory.
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


class EditorObserver:
    """Bounded transport facts only, not a replacement for observing the PTY."""
    def __init__(self, path):
        self.path = path

    def facts(self):
        if not self.path.exists():
            return []
        with self.path.open("rb") as source:
            data = source.read(32769)
        if len(data) > 32768:
            raise AssertionError("editor observer exceeded its 32 KiB bound")
        # A concurrent append can leave an incomplete final record in this read.
        lines = data.rpartition(b"\n")[0].splitlines()
        if len(lines) > 64:
            raise AssertionError("editor observer exceeded its 64-fact bound")
        return [json.loads(line) for line in lines]

    def active_mount(self):
        active = {}
        for fact in self.facts():
            identity = (fact["pid"], fact["surface_id"], fact["mount_id"])
            if fact["event"] == "opened":
                active[identity] = fact
            elif fact["event"] == "closed":
                active.pop(identity, None)
            else:
                raise AssertionError("unknown editor observer fact")
        if len(active) > 1:
            raise AssertionError("editor observer found multiple active editor mounts")
        return next(iter(active.values()), None)

    def rescued(self, mount):
        return any(fact["event"] == "closed"
                   and all(fact[key] == mount[key] for key in ("pid", "surface_id", "mount_id"))
                   and fact["reason"] == "returned to octet with Ctrl+G"
                   for fact in self.facts())


def install_editor_observer(adapter, bundle, root):
    """Preload only the disposable runner; no factory or adapter source edits.

    Transport.request resolves only after a real host reply. Transport.receive
    observes host notifications on both the local and worker-backed transports.
    Worker threads inherit --import, so only the main thread installs the probe.
    No frame/checkpoint logging or extra wait is added to the burst-input path.
    """
    module = root / "editor-observer.mjs"
    trace = root / "editor-observer.jsonl"
    source = r'''
import { isMainThread } from 'node:worker_threads';
import { openSync, writeSync } from 'node:fs';
if (isMainThread) {
  const { Transport } = await import(TRANSPORT);
  const fd = openSync(TRACE, 'a', 0o600), mounts = new Map();
  let facts = 0, bytes = 0;
  const record = fact => {
    const line = JSON.stringify({ pid: process.pid, ...fact }) + '\n';
    const size = Buffer.byteLength(line);
    if (++facts > 64 || (bytes += size) > 32768) throw new Error('editor observer bound exceeded');
    if (writeSync(fd, line) !== size) throw new Error('incomplete editor observer write');
  };
  const identity = value => typeof value === 'string' && /^[A-Za-z0-9_.-]{1,64}$/.test(value);
  const request = Transport.prototype.request;
  Transport.prototype.request = function(method, params, ...args) {
    const reply = request.call(this, method, params, ...args);
    if (method !== 'ui/open' || params.placement !== 'editor') return reply;
    return reply.then(result => {
      if (!identity(params.surface_id) || !identity(result?.editor_mount_id)) {
        throw new Error('editor observer requires a host-acknowledged editor_mount_id');
      }
      mounts.set(params.surface_id, result.editor_mount_id);
      record({ event: 'opened', surface_id: params.surface_id, mount_id: result.editor_mount_id });
      return result;
    });
  };
  const receive = Transport.prototype.receive;
  Transport.prototype.receive = function(message, ...args) {
    if (message.method === 'ui/closed' && message.id === undefined) {
      const p = message.params, mount_id = mounts.get(p.surface_id);
      if (mount_id !== undefined) {
        if (typeof p.reason !== 'string' || Buffer.byteLength(p.reason) > 4096) {
          throw new Error('invalid editor closure observation');
        }
        record({ event: 'closed', surface_id: p.surface_id, mount_id, reason: p.reason });
        mounts.delete(p.surface_id);
      }
    }
    return receive.call(this, message, ...args);
  };
}
'''.replace("TRANSPORT", json.dumps((adapter / "lib/transport.mjs").as_uri())).replace(
        "TRACE", json.dumps(str(trace)))
    module.write_text(source)
    module.chmod(0o600)
    manifest = bundle / "extension.toml"
    lines = manifest.read_text().splitlines(keepends=True)
    positions = [i for i, line in enumerate(lines) if line.startswith("args = ")]
    if len(positions) != 1:
        raise AssertionError("expected one generated adapter argument vector")
    index = positions[0]
    arguments = json.loads(lines[index].removeprefix("args = "))
    if not arguments or Path(arguments[0]) != adapter / "runner.mjs":
        raise AssertionError("observer requires the generated adapter runner")
    lines[index] = "args = " + json.dumps(["--import", module.as_uri(), *arguments]) + "\n"
    manifest.write_text("".join(lines))
    return EditorObserver(trace)


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
        observer = install_editor_observer(args.adapter, extensions / "octet-pi-compat", root)
        terminal = pty.TerminalProcess(pty.native_command(args.binary, extensions), environment, workspace)
        try:
            terminal.until(lambda s: "probe" in s.text() and s.frames > 0 and observer.active_mount() is not None,
                           label="host-acknowledged editor mount and ready terminal frame")
            mount = observer.active_mount()
            frame = terminal.screen.frames
            text = "ultrathink"
            if args.burst:
                terminal.send(text.encode())
            else:
                for char in text:
                    terminal.send(char.encode())
                    terminal.pump(0.012)
            terminal.until(lambda s: s.frames > frame and text in s.text(), label="complete foreign draft frame")
            if not args.burst:
                terminal.pump(0.5)  # grace period, not a protocol checkpoint acknowledgement
                terminal.resize(72, 24)
                terminal.pump(0.3)
            frame = terminal.screen.frames
            terminal.send(b"\x07")
            terminal.until(lambda s: s.frames > frame and observer.rescued(mount) and observer.active_mount() is None,
                           label="matching host Ctrl+G retirement and newer terminal frame")
            if text not in terminal.screen.text():
                raise AssertionError("native rescue lost the complete draft\n" + terminal.screen.text())
            frame = terminal.screen.frames
            terminal.send(b"-native")
            terminal.until(lambda s: s.frames > frame and text + "-native" in s.text()
                           and observer.active_mount() is None,
                           label="retired editor's complete draft remains natively editable")
            # Late composer writes can mutate host text without repainting it.
            # Force a later native edit; do not accept a stale post-rescue frame.
            terminal.pump(0.5)
            frame = terminal.screen.frames
            terminal.send(b"!")
            terminal.until(lambda s: s.frames > frame and text + "-native!" in s.text()
                           and observer.active_mount() is None,
                           label="native draft survives late editor writes")
            if args.burst:
                terminal.resize(72, 24)
                terminal.pump(0.3)
            terminal.send(b"\x03")
            terminal.pump(0.1)
            terminal.close()
            print(json.dumps({"editor": "pass", "entrypoint": str(args.editor),
                "sha256": hashlib.sha256(args.editor.read_bytes()).hexdigest(),
                "mount": mount, "host_rescue_observed": observer.rescued(mount),
                "rescue": "matching host retirement + newer frame + complete native editable draft + later edit",
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
