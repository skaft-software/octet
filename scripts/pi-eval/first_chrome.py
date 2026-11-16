#!/usr/bin/env python3
"""Offline first-chrome/input acceptance with real CLIs and locked Pi reference.

Synthetic factories only. Heavy layout mirrors maintainer_fixture.PACKAGES names,
versions and autoload flags, not those packages' implementations. No installs,
user configuration, credentials, or external providers are used.
"""
from __future__ import annotations
import argparse
import ast
import json
import os
from pathlib import Path
import sys
import time

HERE = Path(__file__).resolve().parent
REPO = HERE.parents[1]
sys.path[:0] = [str(HERE), str(REPO / 'scripts/e2e')]
from run import EvalSession
from fixture import write_provider_config
from octet_e2e.fixtures import build_environment, octet_args
from octet_e2e.provider import MockProvider
from octet_e2e.vt import Screen
from core import terminal_restore

UI = r'''
import { CustomEditor } from '@earendil-works/pi-coding-agent';
export default async pi => {
  await new Promise(resolve => setTimeout(resolve, 150));
  pi.on('session_start', (_, ctx) => {
    ctx.ui.setEditorComponent((tui, theme, keys) => {
      const editor = new CustomEditor(tui, theme, keys, {paddingX: 0});
      editor.borderColor = text => '\x1b[35m' + text + '\x1b[39m';
      return editor;
    });
    ctx.ui.setFooter(() => ({render: () => ['\x1b[35m' + ctx.model.id + ' · 0 context\x1b[39m'], invalidate() {}}));
  });
  pi.on('input', e => ({action: 'transform', text: e.text + '-input-ready'}));
  pi.on('before_agent_start', e => ({systemPrompt: e.systemPrompt + '\nBEFORE-READY'}));
};
'''


def packages():
    tree = ast.parse((HERE / 'maintainer_fixture.py').read_text())
    return next(ast.literal_eval(node.value) for node in tree.body
                if isinstance(node, ast.Assign) and any(isinstance(t, ast.Name) and t.id == 'PACKAGES' for t in node.targets))


def setup(root, target, heavy, port):
    env = build_environment(root, target, port, model='custom/mock-1')
    agent = env.home / '.pi/agent'
    agent.mkdir(parents=True)
    ui = agent / 'chrome.mjs'
    ui.write_text(UI)
    settings = {'extensions': [str(ui)], 'theme': 'dark', 'compaction': {'enabled': False},
                'defaultProvider': 'mock', 'defaultModel': 'mock-1', 'enableInstallTelemetry': False,
                'lastChangelogVersion': '1.5.0', 'defaultProjectTrust': 'never'}
    if heavy:
        entries = []
        for name, version, autoload in packages():
            directory = agent / 'synthetic' / name
            directory.mkdir(parents=True)
            (directory / 'package.json').write_text(json.dumps({'name': name, 'version': version, 'pi': {'extensions': ['./index.mjs']}}))
            (directory / 'index.mjs').write_text("export default async pi => { await new Promise(r => setTimeout(r, 150)); pi.on('input', () => ({action:'continue'})); };\n")
            entries.append({'source': str(directory), 'autoload': autoload})
        settings['packages'] = entries
    (agent / 'settings.json').write_text(json.dumps(settings))
    write_provider_config(env.home, port)
    (env.root / 'tmp').mkdir()
    return env


def frames(session):
    # Replay synchronized frames, not polling snapshots that could miss a jump.
    screen = Screen(120, 40)
    result, pending = [], b''
    for stamp, data in session.chunks:
        pending += data
        while b'\x1b[?2026l' in pending:
            chunk, pending = pending.split(b'\x1b[?2026l', 1)
            screen.feed(chunk + b'\x1b[?2026l')
            result.append({'s': stamp - session.started_at, 'screen': screen.text()})
    return result


def launch(target, env, args, work, provider):
    work.mkdir(parents=True)
    child = {'PATH': os.environ['PATH'], 'HOME': str(env.home), 'TMPDIR': str(env.root / 'tmp'),
             'TERM': 'xterm-256color', 'COLORTERM': 'truecolor', 'LANG': 'C.UTF-8',
             'PI_OFFLINE': '1', 'PI_SKIP_VERSION_CHECK': '1', 'PI_TELEMETRY': '0',
             'OCTET_STARTUP_TRACE': '1', 'OCTET_TERN': 'off'}
    if target == 'octet':
        command = octet_args(env, name=None, extension_dirs=[REPO / 'extensions'], enable_extensions=['octet-pi-compat'],
                            extra=['--tui-mode', 'fullscreen', '--cache-warming', 'off'])
        binary = args.octet
    else:
        command = [str(args.pi_cli), '--model', 'mock/mock-1', '--no-skills']
        binary = Path('node')
    session = EvalSession(binary, args=command, env=child, cwd=env.workspace, columns=120, rows=40,
                          log_path=work / 'capture.ansi', stderr_path=work / 'stderr.log')
    before = len(provider.requests)
    row = {'target': target, 'chrome_s': None, 'typing_s': None, 'submit_s': None, 'acceptance_s': None, 'provider_s': None}
    typed = submitted = False
    try:
        while time.monotonic() - session.started_at < 35:
            elapsed = time.monotonic() - session.started_at
            text = session.screen_text()
            if row['chrome_s'] is None and 'mock-1 · 0 context' in text:
                row['chrome_s'] = elapsed
            # Send typing as soon as the terminal is in raw mode, independently
            # of whether an extension frame has loaded. No echo is readiness.
            if not typed and b'\x1b[?2004h' in session.transcript():
                session.write(b'EARLY-PROMPT')
                typed = True
            if typed and row['typing_s'] is None and 'EARLY-PROMPT' in text:
                row['typing_s'] = elapsed
            if row['typing_s'] is not None and not submitted and (target == 'octet' or (row['chrome_s'] is not None and elapsed > row['chrome_s'] + .05)):
                session.write(b'\r')
                submitted = True
                row['submit_s'] = elapsed
            # Observe the edit leaving the bottom composer, rather than calling
            # the Enter write or eventual provider dispatch acceptance.
            composer = '\n'.join(text.splitlines()[-4:])
            if submitted and row['acceptance_s'] is None and 'EARLY-PROMPT' not in composer:
                row['acceptance_s'] = elapsed
            if len(provider.requests) > before and 'FIRST-CHROME-ANSWER' in text and row['chrome_s'] is not None:
                row['provider_s'] = elapsed
                break
            if session.process.poll() is not None:
                raise AssertionError(f'exited {session.process.returncode}: {session.stderr_text()}')
            time.sleep(.002)
        assert row['provider_s'] is not None, f'no gated prompt: {row}\n{session.screen_text()}\n{session.stderr_text()[-2000:]}'
        assert row['acceptance_s'] is not None, row
        request = provider.requests[before]
        assert 'EARLY-PROMPT-input-ready' in request.user_text(), request.body
        assert 'BEFORE-READY' in json.dumps(request.body), request.body
        time.sleep(.3)
        capture = frames(session)
        (work / 'frames.json').write_text(json.dumps(capture, indent=2))
        row['first_frame_s'] = capture[0]['s'] if capture else None
        row['first_frame_pi_chrome'] = bool(capture and 'mock-1 · 0 context' in capture[0]['screen'] and '─' in capture[0]['screen'])
        positions, borders = [], []
        for frame in capture:
            lines = frame['screen'].splitlines()
            if any('mock-1 · 0 context' in line for line in lines):
                positions.append(next(i for i, line in enumerate(lines) if 'mock-1 · 0 context' in line))
                borders.append(tuple(i for i, line in enumerate(lines) if '─' * 30 in line))
        painted = [i for i, frame in enumerate(capture) if 'mock-1 · 0 context' in frame['screen']]
        row['footer_missing_frames'] = sum('mock-1 · 0 context' not in capture[i]['screen'] for i in range(painted[0], painted[-1] + 1))
        row['composer_row_positions'] = sorted(set(borders))
        row['footer_row_positions'] = sorted(set(positions))
        session.write(b'\x03')
        row['exit_code'] = session.close(timeout=8)
        row['restored'] = terminal_restore(session.transcript())['restored']
        assert row['exit_code'] == 0 and row['restored'], row
        (work / 'result.json').write_text(json.dumps(row, indent=2))
        return row
    finally:
        if session.process.poll() is None:
            session.kill()


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--octet', type=Path, required=True)
    parser.add_argument('--pi-cli', type=Path, default=REPO / 'scripts/fixtures/pi-reference/node_modules/pi-session-1-5-0/dist/cli.js')
    parser.add_argument('--out', type=Path, required=True)
    args = parser.parse_args()
    args.octet = args.octet.resolve(strict=True)
    args.pi_cli = args.pi_cli.resolve(strict=True)
    args.out = args.out.resolve()
    args.out.mkdir(parents=True, exist_ok=False)
    provider = MockProvider()
    provider.default_reply('FIRST-CHROME-ANSWER')
    rows = []
    try:
        for heavy in (False, True):
            profile = 'heavy-synthetic' if heavy else 'pi-reference'
            for target in ('pi', 'octet'):
                env = setup(args.out / profile / 'homes', target, heavy, provider.port)
                for leg in ('cold', 'warm'):
                    row = launch(target, env, args, args.out / profile / target / leg, provider)
                    row.update(profile=profile, leg=leg)
                    if target == 'octet' and leg == 'warm':
                        assert row['first_frame_pi_chrome'], row
                        assert row['footer_missing_frames'] == 0, row
                        assert row['footer_row_positions'] == [39], row
                        assert row['composer_row_positions'] == [(36, 38)], row
                    rows.append(row)
                    print(json.dumps(row), flush=True)
                    (args.out / 'results.json').write_text(json.dumps(rows, indent=2))
    finally:
        provider.close()


if __name__ == '__main__':
    main()
