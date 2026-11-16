#!/usr/bin/env python3
"""Offline PTY startup handoff audit (Linux strace records every stdout write).

Cold means a fresh HOME, not dropped OS page cache. Warm reuses that HOME.
No inference is submitted; the manual loopback model never contacts a provider.
The optional API 0.4 fixture exercises process/handshake/hooks, not Pi loading.
"""
import argparse
import ast
import fcntl
import hashlib
import json
import os
from pathlib import Path
import pty
import re
import select
import signal
import struct
import statistics
import subprocess
import tempfile
import termios
import time

BEGIN, END = b"\x1b[?2026h", b"\x1b[?2026l"
PHASE = re.compile(r"octet-startup: ([\w.:-]+) elapsed=(\d+)us")
PROBE = '''#!/usr/bin/python3
import json, sys
for line in sys.stdin:
    r = json.loads(line)
    if "id" not in r or "method" not in r: continue
    m = r["method"]
    if m == "initialize":
        p = r["params"]["protocol"]
        result = {"api_version":"0.4", "tools":[], "commands":[],
            "protocol":{"version":"0.4", "features":p["required_features"],
                "limits":{"max_concurrent_requests":2}}}
    else: result = {}
    print(json.dumps({"jsonrpc":"2.0", "id":r["id"], "result":result}), flush=True)
    if m == "shutdown": break
'''


def prepare(home, extension, theme):
    config = home / ".octet"
    (config / "credentials").mkdir(parents=True)
    (config / "credentials/custom.json").write_text(json.dumps({
        "base_url": "http://127.0.0.1:9/v1/", "api_key": "", "api_name": "startup-probe",
        "headers": [], "auto_discover": False,
        "models": [{"api_name": "startup-probe", "context_window": 32768}]}))
    (config / "credentials/custom.json").chmod(0o600)
    (config / "config.toml").write_text(f'theme = "{theme}"\nmodel = "custom/startup-probe"\n'
        + ('enabled_extensions = ["startup-probe"]\n' if extension else ''))
    if extension:
        bundle = config / "extensions/startup-probe"
        bundle.mkdir(parents=True)
        (bundle / "probe").write_text(PROBE)
        (bundle / "probe").chmod(0o700)
        (bundle / "extension.toml").write_text('''name = "startup-probe"
version = "0.1.0"
api_version = "0.4"
[entrypoint]
command = "probe"
[contributes]
hooks = ["session_start", "session_end"]
''')


def sample(binary, home, artifact, extension, wait_submission):
    master, slave = pty.openpty()
    fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack('HHHH', 24, 80, 0, 0))
    original = termios.tcgetattr(slave)
    output = bytearray()
    observations = []
    started = time.time_ns() / 1e9
    def controlling_tty():
        os.setsid()
        fcntl.ioctl(0, termios.TIOCSCTTY, 0)

    with open(str(artifact) + '.stderr', 'wb') as errors:
        child = subprocess.Popen(['strace', '-ff', '-ttt', '-s', '1048576', '-e', 'trace=write,writev,execve,clone,clone3',
            '-o', str(artifact), str(binary), '--offline', '--no-context-files', '--no-tools', '--tern', 'off'],
            stdin=slave, stdout=slave, stderr=errors, cwd=home,
            env={'HOME': str(home), 'PATH': '/usr/bin:/bin', 'TERM': 'xterm-256color',
                 'COLORTERM': 'truecolor', 'LANG': 'C.UTF-8', 'OCTET_STARTUP_TRACE': '1'},
            preexec_fn=controlling_tty)
        try:
            deadline = time.monotonic() + 30
            ready = False
            while time.monotonic() < deadline:
                if select.select([master], [], [], .01)[0]:
                    data = os.read(master, 65536)
                    output.extend(data)
                    observations.append([round((time.time() - started)*1000, 3), len(output)])
                ready = b'startup-probe' in output and END in output[output.find(b'startup-probe'):]
                # Let background handshake/frame settlement occur without timing it
                # by a sleep: the exit fence is the actual settled trace where available.
                traces = Path(str(artifact) + '.stderr').read_text()
                settled = ('frame.settled-written' in traces or 'extensions.hooks-settled' in traces
                           or (not extension and ready))
                if ready and settled and (not wait_submission or 'submission.ready' in traces):
                    break
            if not ready:
                raise RuntimeError(f'no ready frame: {output[-500:]!r}; {traces}')
            os.write(master, b'edit-probe')
            deadline = time.monotonic() + 5
            while b'edit-probe' not in output and time.monotonic() < deadline:
                if select.select([master], [], [], .01)[0]: output.extend(os.read(master, 65536))
            if b'edit-probe' not in output: raise RuntimeError('no application-handled edit')
            os.write(master, b'\x04')
            deadline = time.monotonic() + 4
            while child.poll() is None and time.monotonic() < deadline:
                if select.select([master], [], [], .01)[0]: output.extend(os.read(master, 65536))
            child.wait(timeout=2)
            if child.returncode: raise RuntimeError(f'exit {child.returncode}')
            if termios.tcgetattr(slave) != original: raise RuntimeError('terminal modes not restored')
        finally:
            if child.poll() is None:
                os.killpg(child.pid, signal.SIGKILL)
                child.wait()
            os.close(master)
            os.close(slave)
    writes = []
    logs = {int(path.suffix[1:]): path.read_text().splitlines()
            for path in artifact.parent.glob(artifact.name + '.*') if path.suffix[1:].isdigit()}
    # fd 1 in an extension is its protocol pipe, not the PTY. Follow only
    # octet's CLONE_THREAD children, never forked extension/process stdout.
    main_pid = next(pid for pid, lines in logs.items()
                    if any(f'execve("{binary}"' in line for line in lines))
    tids = {main_pid}
    while True:
        children = {int(match[1]) for tid in tids for line in logs.get(tid, [])
                    if (match := re.search(r'clone(?:3)?\(.*CLONE_THREAD.* = (\d+)', line))}
        if children <= tids: break
        tids |= children
    process_enter_ms = None
    for tid in tids:
        for line in logs.get(tid, []):
            if 'write(2, "octet-startup: process.enter elapsed=0us' in line:
                process_enter_ms = (float(line.split()[0])-started)*1000
            if re.search(r'writev\(1,', line):
                raise RuntimeError('stdout writev needs explicit decoding; capture is incomplete')
            match = re.match(r'(\d+\.\d+) write\(1, ("(?:[^"\\]|\\.)*"), \d+\)\s+= (\d+)', line)
            if match:
                # strace's octal escapes and ordinary ASCII are Python literals.
                data = ast.literal_eval('b' + match[2])[:int(match[3])]
                writes.append((float(match[1]), data))
    writes.sort(key=lambda item: item[0])
    wire = b''
    first = title = ready = None
    empty_frames = 0
    (Path(str(artifact) + '.writes.json')).write_text(json.dumps([
        {'ms': round((timestamp-started)*1000, 3), 'data': list(data)} for timestamp, data in writes]))
    for timestamp, data in writes:
        wire += data
        ms = round((timestamp - started)*1000, 3)
        if title is None and (b'\x1b]2;' in data or b'\x1b]0;' in data): title = ms
        if first is None and END in wire: first = ms
        if ready is None and b'startup-probe' in wire and END in wire[wire.find(b'startup-probe'):]: ready = ms
    for frame in wire.split(END)[:-1]:
        if b'startup-probe' in frame: break
        empty_frames += 1
    phases = {name: int(us)/1000 for name, us in PHASE.findall(Path(str(artifact)+'.stderr').read_text())}
    return {'first_frame_ms': first, 'ready_frame_ms': ready, 'title_ms': title,
            'title_after_ready_ms': None if title is None or ready is None else round(title-ready, 3),
            'unresolved_transactions': empty_frames, 'phase_ms': phases, 'stdout_writes': len(writes),
            'process_enter_ms': process_enter_ms,
            'phase_spawn_ms': {name: round(process_enter_ms+ms, 3) for name, ms in phases.items()}
                if process_enter_ms is not None else {},
            'observations': observations}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('binaries', type=Path, nargs='+')
    parser.add_argument('--trials', type=int, default=3)
    parser.add_argument('--theme', choices=['dark', 'auto'], default='dark')
    parser.add_argument('--output', type=Path, required=True)
    args = parser.parse_args()
    args.output = args.output.resolve()
    args.output.mkdir(parents=True, exist_ok=False)
    binaries = [p.resolve() for p in args.binaries]
    rows = []
    for extension in [False, True]:
        for trial in range(args.trials):
            with tempfile.TemporaryDirectory(prefix='octet-handoff-') as tmp:
                for binary in binaries[::1 if trial % 2 == 0 else -1]:
                    home = Path(tmp) / binary.name
                    prepare(home, extension, args.theme)
                    for temperature in ['cold', 'warm']:
                        name = f'{binary.name}-{extension}-{trial}-{temperature}'
                        row = {'binary': binary.name, 'extension': extension, 'trial': trial, 'theme': args.theme,
                               'temperature': temperature, **sample(binary, home, args.output / name, extension, args.theme == 'auto')}
                        rows.append(row)
                        print(json.dumps(row), flush=True)
    (args.output / 'summary.json').write_text(json.dumps({'sha256': {
        p.name: hashlib.sha256(p.read_bytes()).hexdigest() for p in binaries}, 'runs': rows}, indent=2))
    for binary in binaries:
        for extension in [False, True]:
            for temp in ['cold', 'warm']:
                cell = [r for r in rows if r['binary']==binary.name and r['extension']==extension and r['temperature']==temp]
                print(binary.name, extension, temp, {key: statistics.median(r[key] for r in cell)
                    for key in ['first_frame_ms', 'ready_frame_ms', 'title_after_ready_ms', 'unresolved_transactions']})


if __name__ == '__main__':
    main()
