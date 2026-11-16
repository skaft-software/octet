#!/usr/bin/env python3
"""Offline PTY qualification of an explicitly supplied unchanged Whimsical 0.5.4.

No downloads, credentials or live providers. Fetch/review the npm tarball separately.
Uses real Pi/octet processes, a gated loopback provider and native terminal output.
"""
import argparse
import hashlib
import importlib.util
import json
import os
import re
from pathlib import Path
import shutil
import sys
import threading
import time

ROOT = Path(__file__).resolve().parents[1]
sys.path[:0] = [str(ROOT / "scripts/e2e"), str(ROOT / "scripts/pi-eval")]
from octet_e2e.provider import MockProvider, Reply
from fixture import write_provider_config

spec = importlib.util.spec_from_file_location("pi_pty", ROOT / "scripts/test-pi-compat-pty.py")
pty = importlib.util.module_from_spec(spec)
spec.loader.exec_module(pty)
PRESETS = {
    "sleekOrbit": ["◜", "◠", "◝", "◞", "◡", "◟"],
    "neonPulse": ["∙∙●∙∙", "∙●∙●∙", "●∙∙∙●"],
    "scanline": ["▏", "▎", "▍", "▌", "▋", "▊", "▉", "█"],
    "chevronFlow": [">>>", ">>·", ">··", "···", "·<<", "<<<"],
    "matrixGlyph": ["┆", "╎", "┊", "╏", "┋"],
}


class ColorScreen(pty.Screen):
    """Retain indexed/basic foreground identity as well as the RGB observer."""
    def csi(self, final):
        previous = self.fg
        super().csi(final)
        if final != "m":
            return
        codes = [int(value) if value else 0 for value in self.sequence.split(";")]
        color, index = previous, 0
        while index < len(codes):
            code = codes[index]
            if code in (0, 39):
                color = None
            elif 30 <= code <= 37 or 90 <= code <= 97:
                color = ("ansi", code)
            elif code in (38, 48) and index + 1 < len(codes):
                mode = codes[index + 1]
                length = 3 if mode == 2 else 1
                if code == 38:
                    color = tuple(codes[index + 2:index + 5]) if mode == 2 else ("indexed", codes[index + 2])
                index += length + 1
            index += 1
        self.fg = color


class TernProcess(pty.TerminalProcess):
    """Bounded protocol observer, not a terminal renderer or pixel oracle."""
    def __init__(self, *args, **kwargs):
        self.pending = b""
        self.offset = 0
        self.nodes, self.parents = {}, {}
        super().__init__(*args, **kwargs)

    def event(self, body, verb="e"):
        self.send(b"\x1b_tsp;" + verb.encode() + b";" + json.dumps(body).encode() + b"\x1b\\")

    def add(self, node, parent):
        self.nodes[node["id"]] = node
        self.parents[node["id"]] = parent
        for child in node.get("c") or []:
            self.add(child, node["id"])

    def delete(self, node_id):
        for child in [child for child, parent in self.parents.items() if parent == node_id]:
            self.delete(child)
        self.nodes.pop(node_id, None)
        self.parents.pop(node_id, None)

    def pump(self, seconds=.05):
        super().pump(seconds)
        self.pending += self.capture[self.offset:]
        self.offset = len(self.capture)
        while True:
            start = self.pending.find(b"\x1b_tsp;")
            end = self.pending.find(b"\x1b\\", start) if start >= 0 else -1
            if end < 0:
                break
            sequence = self.pending[start + 6:end]
            self.pending = self.pending[end + 2:]
            verb, body = sequence.split(b";", 1)
            if verb == b"b":
                continue
            body = json.loads(body)
            if verb == b"q" and body.get("q") == "hello":
                kinds = ["col", "row", "card", "section", "rule", "spacer", "text", "md", "code", "diff",
                         "ansi", "math", "image", "kv", "table", "tree", "badge", "kbd", "icon", "spinner",
                         "shimmer", "elapsed", "progress", "rate", "list", "item", "tabs", "editor", "input",
                         "status", "seg", "overlay", "toast", "rows", "picker", "prefs", "tool", "checklist",
                         "agent", "chart", "meter", "effort"]
                self.event({"r": "hello", "v": 1, "term": "loader-test", "kinds": kinds,
                            "features": [], "apc": 65536, "credits": 2, "cols": 110,
                            "dark": True, "reduceMotion": True}, verb="r")
            elif verb == b"o":
                self.nodes.clear(); self.parents.clear()
            elif verb == b"f":
                for op in body["ops"]:
                    if op[0] == "add":
                        self.add(op[4], op[2])
                    elif op[0] == "del":
                        self.delete(op[1])
                    elif op[0] == "set":
                        self.nodes[op[1]].setdefault("p", {}).update(op[2])
                    elif op[0] == "move":
                        self.parents[op[1]] = op[2]
                self.event({"ev": "ack", "sf": body["sf"], "s": body["s"]})
            assert len(self.nodes) <= 4096, "bounded test tree exceeded"
        assert len(self.pending) <= 1048576, "bounded TSP observer exceeded"


def leg(args, target, preset, static=False, tern=False):
    root = args.output / (target + ("-tern" if tern else "-static" if static else "")) / preset
    root.mkdir(parents=True)
    home, work, tmp = (root / name for name in ("home", "work", "tmp"))
    for path in (home, work, tmp):
        path.mkdir()
    provider = MockProvider()
    gate = threading.Event()
    provider.reply_with(lambda _: True, Reply(gate=gate, gate_timeout=180))
    write_provider_config(home, provider.port)
    agent = home / ".pi/agent"
    companion = root / "controls.mjs"
    control = root / "control.json"
    companion.write_text("import { readFileSync, writeFileSync, appendFileSync } from 'node:fs';\nconst control = " + json.dumps(str(control)) + ";\n" + '''export default pi => {
      let timer, seen = 0;
      pi.on('session_start', (_event, ctx) => {
        appendFileSync(control + '.owners', String(process.pid) + '\\n');
        try { seen = JSON.parse(readFileSync(control, 'utf8')).seq; } catch {}
        timer = setInterval(() => {
          let value; try { value = JSON.parse(readFileSync(control, 'utf8')); } catch { return; }
          if (value.seq <= seen) return;
          seen = value.seq;
          const args = value.action;
          if (args === 'start') pi.sendUserMessage('offline loader tuner work');
          else if (args === 'hide') ctx.ui.setWorkingVisible(false);
          else if (args === 'show') ctx.ui.setWorkingVisible(true);
          else if (args === 'explicit') ctx.ui.setWorkingIndicator({frames:['EXPLICIT'], intervalMs:80});
          else if (args === 'empty') ctx.ui.setWorkingIndicator({frames:[]});
          else if (args === 'reset') ctx.ui.setWorkingIndicator();
          else if (args === 'light') ctx.ui.setTheme('light');
          else if (args === 'dark') ctx.ui.setTheme('dark');
          else ctx.ui.setWorkingMessage(args || undefined);
          writeFileSync(control + '.ack', String(seen));
        }, 50);
      });
      pi.on('session_shutdown', () => clearInterval(timer));
    };''')
    (agent / "settings.json").write_text(json.dumps({
        "extensions": [str(args.whimsical), str(companion)], "theme": "dark",
        "whimsical": {"spinnerPreset": preset}, "compaction": {"enabled": False},
        "lastChangelogVersion": "1.5.0", "enableInstallTelemetry": False,
    }))
    env = {"PATH": os.environ["PATH"], "HOME": str(home), "TMPDIR": str(tmp),
           "TERM": "xterm-256color", "COLORTERM": "truecolor", "LANG": "C.UTF-8",
           "PI_OFFLINE": "1", "PI_SKIP_VERSION_CHECK": "1", "PI_TELEMETRY": "0",
           "PI_CODING_AGENT_DIR": str(agent), "OCTET_PI_AGENT_DIR": str(agent),
           "XDG_CACHE_HOME": str(tmp / "cache")}
    if tern:
        env["TERM_PROGRAM"] = "tern"
    if target == "octet":
        extensions = pty.configure(ROOT / "extensions/octet-pi-compat", Path(shutil.which("node")),
                                   root, [args.whimsical, companion], env, work)
        command = [str(args.binary), "--offline", "--model", "custom/mock-1", "--theme", "pi",
                   "--no-context-files", "--no-tools", "--cache-warming", "off",
                   "--extension-dir", str(extensions), "--enable-extension", "octet-pi-compat"]
        if static:
            command += ["--color", "never"]
    else:
        command = [shutil.which("node"), str(args.pi_cli), "--model", "mock/mock-1", "--no-session", "--no-skills"]
    terminal = (TernProcess if tern else pty.TerminalProcess)(command, env, work, columns=110, rows=42)
    terminal.screen = ColorScreen(110, 42, track_colors=True)
    checks = []
    sequence = 0

    def send_control(value):
        nonlocal sequence
        sequence += 1
        control.write_text(json.dumps({"seq": sequence, "action": value}))
        def acknowledged(_):
            try:
                return Path(str(control) + ".ack").read_text() == str(sequence)
            except FileNotFoundError:
                return False
        terminal.until(acknowledged, label="retained control " + value)
        terminal.pump(.2)

    def check(name, predicate, timeout=20):
        terminal.until(predicate, timeout=timeout, label=name)
        checks.append(name)
        (root / (str(len(checks)) + ".screen")).write_text(terminal.screen.text())

    def send_command(value):
        terminal.send((value + "\r").encode())
        terminal.pump(.3)

    def stop_work():
        terminal.send(b"\x1b")
        terminal.pump(.4)
        terminal.until(lambda s: not any(re.search(r"(?:^|\s)" + re.escape(frame) + r"(?:\s|$)", "\n".join(s.text().splitlines()[8:]), re.M)
                                       for frames in PRESETS.values() for frame in frames),
                       label="no working component after cancellation")
        checks.append("cancel stops working component")

    def start_work(text):
        requests = len(provider.requests)
        send_command(text)
        terminal.until(lambda _: len(provider.requests) > requests, label="new provider request")
        terminal.pump(.2)

    def adapter_pid():
        return int(Path(str(control) + ".owners").read_text().splitlines()[-1])

    def gone(pid):
        try:
            os.kill(pid, 0)
            return False
        except ProcessLookupError:
            return True

    def spinner_color(screen):
        for row, cells in enumerate(screen.grid):
            text = "".join(cells)
            if "LOADER-PROBE" in text:
                column = next(i for i, glyph in enumerate(cells) if glyph.strip())
                return screen.colors[row][column][0]
        return None

    def indicator(screen, frames=None, message="LOADER-PROBE"):
        return any(message in line and any(frame in line for frame in (frames or PRESETS[preset]))
                   for line in screen.text().splitlines())

    try:
        if tern:
            check("Tern reduced-motion handshake", lambda _: "composer.editor" in terminal.nodes)
            terminal.pump(1)
            start_work("offline Tern reduced motion")
            send_control("LOADER-PROBE")
            check("reduced-motion static native row", lambda _: terminal.nodes.get("work.spin", {}).get("k") == "text"
                  and "LOADER-PROBE" in json.dumps(terminal.nodes.get("work.label", {})))
            assert not any(frame in json.dumps(terminal.nodes.get("work.spin", {}), ensure_ascii=False)
                           for frame in PRESETS[preset])
            terminal.event({"ev": "motion", "reduce": False})
            check("motion-enabled effective component", lambda _: terminal.nodes.get("work.spin", {}).get("k") == "ansi"
                  and "LOADER-PROBE" in terminal.nodes.get("work.spin", {}).get("p", {}).get("text", ""))
            assert "work.label" not in terminal.nodes, "duplicate native message"
            line = terminal.nodes["work.spin"]["p"]["text"]
            assert any(frame in line for frame in PRESETS[preset]), line
            terminal.event({"ev": "motion", "reduce": True})
            check("live reduced-motion toggle", lambda _: terminal.nodes.get("work.spin", {}).get("k") == "text"
                  and "work.label" in terminal.nodes)
            (root / "reduced-tree.json").write_text(json.dumps(terminal.nodes, indent=2))
            terminal.send(b"\x1b")
            check("Tern cancellation removes row", lambda _: "work.spin" not in terminal.nodes)
            last_pid = adapter_pid()
            gate.set(); terminal.close()
            assert gone(last_pid), "adapter survived Tern exit"
            checks.append("timer cleanup and terminal restoration")
            return {"target": "octet-tern", "preset": preset, "checks": checks, "status": "pass"}
        check("ready", lambda s: "mock-1" in s.text())
        terminal.pump(1)
        start_work("offline loader work")
        if static:
            send_control("LOADER-PROBE")
            check("static terminal message", lambda s: "LOADER-PROBE" in s.text())
            rows = [line for line in terminal.screen.text().splitlines() if "LOADER-PROBE" in line]
            assert not any(frame in "\n".join(rows) for frame in PRESETS[preset]), rows
            checks.append("terminal animation policy suppresses component")
            stop_work()
            gate.set(); terminal.close()
            return {"target": "octet-static", "preset": preset, "checks": checks, "status": "pass"}
        check("original preset", lambda s: any(frame in s.text() for frame in PRESETS[preset]))
        send_control("LOADER-PROBE")
        check("effective message and preset", indicator)
        terminal.pump(.25)
        rows = [line for line in terminal.screen.text().splitlines() if "LOADER-PROBE" in line]
        assert len(rows) == 1, rows
        assert not any(glyph in rows[0] for glyph in "⠋⠙⠹⠸⠼⠴⠦⠧⠇⠏"), rows
        checks.append("one spinner")
        dark_color = spinner_color(terminal.screen)
        send_control("light")
        check("theme change", indicator)
        light_color = spinner_color(terminal.screen)
        assert dark_color is not None and light_color is not None, (dark_color, light_color)
        assert dark_color != light_color, (dark_color, light_color)
        checks.append("effective spinner color follows palette")
        send_control("dark")
        check("theme restore", indicator)
        send_control("explicit")
        # Whimsical patches updateDisplay, replacing even explicit frames in Pi.
        check("explicit options retain prototype behavior", indicator)
        send_control("empty")
        check("empty options retain prototype behavior", indicator)
        send_control("reset")
        check("indicator reset", indicator)
        send_control("hide")
        check("hidden", lambda s: not any("LOADER-PROBE" in line for line in s.text().splitlines()))
        send_control("show")
        check("shown again", indicator)
        # Open the unmodified tuner, then start work via Pi's retained-context
        # sendUserMessage API. Octet deliberately queues busy slash commands.
        stop_work()
        send_command("/whimsy")
        check("original tuner", lambda s: "Whimsy Chaos Mixer" in s.text())
        requests = len(provider.requests)
        send_control("start")
        check("work starts with original tuner open", lambda _: len(provider.requests) > requests)
        for _ in range(7):
            terminal.send(b"\x1b[B"); terminal.pump(.04)
        terminal.send(b"\x1b[C"); terminal.pump(.1); terminal.send(b"\r")
        next_preset = list(PRESETS)[(list(PRESETS).index(preset) + 1) % len(PRESETS)]
        check("preset changes while working", lambda s: "Whimsy Chaos Mixer" not in s.text()
              and any(frame in s.text() for frame in PRESETS[next_preset]))
        stop_work()
        start_work("offline loader restart")
        check("restart", lambda s: any(frame in s.text() for frame in PRESETS[next_preset]))
        stop_work()
        old_pid = adapter_pid()
        send_command("/reload")
        terminal.pump(2)
        if target == "octet":
            check("reload retires old adapter timers", lambda _: adapter_pid() != old_pid and gone(old_pid))
        start_work("offline loader after reload")
        check("reload restarts component", lambda s: any(frame in s.text() for frame in PRESETS[next_preset]))
        stop_work()
        last_pid = adapter_pid()
        gate.set()
        terminal.close()
        if target == "octet":
            assert gone(last_pid), "adapter with animation timers survived native exit"
        checks.append("coordinated exit, timer cleanup and terminal restoration")
        return {"target": target, "preset": preset, "checks": checks, "status": "pass"}
    finally:
        gate.set()
        (root / "capture.pty").write_bytes(terminal.capture)
        (root / "last.screen").write_text(terminal.screen.text())
        terminal.dispose()
        provider.close()


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", type=Path, required=True)
    parser.add_argument("--pi-cli", type=Path, required=True)
    parser.add_argument("--whimsical", type=Path, required=True)
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--target", choices=["pi", "octet", "both"], default="both")
    parser.add_argument("--preset", choices=list(PRESETS))
    args = parser.parse_args()
    for key in ("binary", "pi_cli", "whimsical"):
        setattr(args, key, getattr(args, key).resolve(strict=True))
    args.output = args.output.resolve()
    args.output.mkdir(exist_ok=False)
    results = []
    try:
        for target in (["pi", "octet"] if args.target == "both" else [args.target]):
            for preset in ([args.preset] if args.preset else PRESETS):
                result = leg(args, target, preset)
                results.append(result)
                print(json.dumps(result), flush=True)
        if args.target in ("octet", "both"):
            result = leg(args, "octet", args.preset or "neonPulse", static=True)
            results.append(result)
            print(json.dumps(result), flush=True)
            result = leg(args, "octet", args.preset or "neonPulse", tern=True)
            results.append(result)
            print(json.dumps(result), flush=True)
    finally:
        (args.output / "results.json").write_text(json.dumps({
            "whimsical_sha256": hashlib.sha256(args.whimsical.read_bytes()).hexdigest(),
            "binary_sha256": hashlib.sha256(args.binary.read_bytes()).hexdigest(),
            "results": results}, indent=2))


if __name__ == "__main__":
    main()
