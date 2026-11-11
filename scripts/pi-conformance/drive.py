#!/usr/bin/env python3
"""PTY driver shared by conformance.py: the real octet binary with the Pi adapter enabled, on a PTY.

Everything is isolated (HOME, workspace, extension dir, a managed Pi release that points at
PI_ROOT) and offline (a loopback mock provider). The screen is a real VT replay of the PTY bytes.
Not a test module; see README.md.
"""
import fcntl, http.server, json, os, pty, select, shutil, signal, struct, subprocess, sys, tempfile, termios, threading, time

HERE = os.path.dirname(os.path.abspath(__file__))
# PI_ROOT: a directory whose node_modules holds @earendil-works/pi-coding-agent (Pi 1.5.0) and @xterm/headless;
# by default this directory (`npm install --prefix scripts/pi-conformance`).
PI_ROOT = os.environ.get('E2E_PI_ROOT') or HERE
THIN = PI_ROOT
ADAPTER = os.environ.get('E2E_ADAPTER') or os.path.join(os.path.dirname(os.path.dirname(HERE)), 'extensions/octet-pi-compat')
PI = os.path.join(PI_ROOT, 'node_modules/@earendil-works/pi-coding-agent')
EXAMPLES = os.path.join(PI, 'examples/extensions')
NODE_BIN = os.environ.get('E2E_NODE_BIN') or os.path.dirname(shutil.which('node') or '/usr/bin/node')
OCTET = os.environ.get('E2E_OCTET') or os.path.join(os.path.dirname(os.path.dirname(HERE)), 'target/release/octet')
COLUMNS, ROWS = 120, 32
DEFAULT_EXAMPLES = ['hello.ts', 'commands.ts', 'status-line.ts', 'custom-footer.ts', 'widget-placement.ts', 'session-name.ts']


class MockProvider:
    """Loopback OpenAI-compatible endpoint: first reply calls the `hello` tool, the next one finishes."""

    def __init__(self, tool='hello', tool_args=None):
        self.requests = []
        self.tool, self.tool_args = tool, tool_args or {'name': 'octet'}
        outer = self

        class Handler(http.server.BaseHTTPRequestHandler):
            def log_message(self, *args):
                pass

            def do_POST(self):
                body = json.loads(self.rfile.read(int(self.headers['Content-Length'])))
                outer.requests.append(body)
                called = any(m.get('role') == 'tool' for m in body.get('messages', []))
                offered = any(t.get('function', {}).get('name') == outer.tool for t in body.get('tools') or [])
                if called or not offered:
                    delta = {'role': 'assistant', 'content': 'all done'}
                    finish = 'stop'
                else:
                    delta = {'role': 'assistant', 'tool_calls': [{'index': 0, 'id': 'call_1', 'type': 'function',
                             'function': {'name': outer.tool, 'arguments': json.dumps(outer.tool_args)}}]}
                    finish = 'tool_calls'
                chunks = [{'id': 'm', 'choices': [{'index': 0, 'delta': delta, 'finish_reason': None}]},
                          {'id': 'm', 'choices': [{'index': 0, 'delta': {}, 'finish_reason': finish}],
                           'usage': {'prompt_tokens': 1, 'completion_tokens': 1, 'total_tokens': 2}}]
                payload = ''.join(f'data: {json.dumps(c)}\n\n' for c in chunks) + 'data: [DONE]\n\n'
                self.send_response(200)
                self.send_header('content-type', 'text/event-stream')
                self.end_headers()
                self.wfile.write(payload.encode())

        self.server = http.server.ThreadingHTTPServer(('127.0.0.1', 0), Handler)
        self.url = f'http://127.0.0.1:{self.server.server_address[1]}/v1/'
        threading.Thread(target=self.server.serve_forever, daemon=True).start()

    def close(self):
        self.server.shutdown()


class Env:
    def __init__(self, mode, examples=DEFAULT_EXAMPLES, provider_url='http://127.0.0.1:9/v1/'):
        self.mode = mode
        self.root = tempfile.mkdtemp(prefix=f'e2e-{mode}-', dir=os.environ.get('E2E_TMP', tempfile.gettempdir()))
        self.home = os.path.join(self.root, 'home')
        self.ws = os.path.join(self.root, 'ws')
        self.ext = os.path.join(self.root, 'ext')
        self.agent = os.path.join(self.home, '.pi/agent')  # the extension sees only HOME, not OCTET_PI_AGENT_DIR
        for d in (self.home + '/.octet/credentials', self.ws + '/.pi', self.ext, self.agent + '/install/releases'):
            os.makedirs(d, exist_ok=True)
        open(self.ws + '/.pi/settings.json', 'w').write('{}')
        version = json.load(open(PI + '/package.json'))['version']
        open(self.agent + '/install/current-version', 'w').write(version + '\n')
        os.symlink(THIN, f'{self.agent}/install/releases/{version}')
        credential = self.home + '/.octet/credentials/custom.json'
        json.dump({'base_url': provider_url, 'api_key': '', 'api_name': 'probe', 'headers': [],
                   'auto_discover': False, 'models': [{'api_name': 'probe', 'context_window': 32768}]}, open(credential, 'w'))
        os.chmod(credential, 0o600)
        self.env = {'PATH': NODE_BIN + ':/usr/bin:/bin', 'HOME': self.home, 'PWD': self.ws, 'TERM': 'xterm-256color', 'LANG': 'C.UTF-8',
                    'PI_OFFLINE': '1', 'COLUMNS': str(COLUMNS), 'LINES': str(ROWS)}
        paths = [name if os.path.isabs(name) else os.path.join(EXAMPLES, name) for name in examples]
        done = subprocess.run(['node', ADAPTER + '/configure.mjs', '--reviewed', '--output', self.ext + '/octet-pi-compat', *paths],
                              cwd=self.ws, env=self.env, capture_output=True, text=True)
        self.configure_output = done.stdout + done.stderr
        if done.returncode != 0:
            raise SystemExit('configure failed: ' + self.configure_output)

        self.wire = os.environ.get('E2E_WIRE')
        if self.wire:  # route the extension through a transparent tee, then log both directions
            toml = self.ext + '/octet-pi-compat/extension.toml'
            text = open(toml).read()
            import re
            command = re.search(r'^command = (".*")$', text, re.M).group(1)
            args = re.search(r'^args = (\[.*\])$', text, re.M).group(1)
            tap = json.dumps([HERE + '/wire-tap.mjs', self.wire + '.' + mode, json.loads(command)] + json.loads(args))
            text = text.replace('command = ' + command, 'command = ' + json.dumps(NODE_BIN + '/node')).replace('args = ' + args, 'args = ' + tap)
            open(toml, 'w').write(text)

    def args(self, *extra):
        return [OCTET, '--offline', '--no-context-files', '--color', 'never', '--model', 'custom/probe', '--workspace', self.ws,
                '--workspace-trusted', '--extension-dir', self.ext, '--enable-extension', 'octet-pi-compat',
                '--trust-extension', 'octet-pi-compat', *extra]

    def cleanup(self):
        shutil.rmtree(self.root, ignore_errors=True)


class Term:
    def __init__(self, env, *extra):
        self.env = env
        self.raw = env.root + '/raw.bin'
        self.log = open(self.raw, 'wb')
        pid, fd = pty.fork()
        if pid == 0:
            os.chdir(env.ws)
            os.execve('/usr/bin/taskset', ['taskset', '-c', os.environ['E2E_CPUS'], OCTET, *env.args(*extra)[1:]], env.env) if os.environ.get('E2E_CPUS') else os.execve(getattr(env, 'exe', OCTET), env.args(*extra), env.env)
        self.pid, self.fd = pid, fd
        fcntl.ioctl(fd, termios.TIOCSWINSZ, struct.pack('HHHH', ROWS, COLUMNS, 0, 0))
        os.kill(pid, signal.SIGWINCH)

    def pump(self, seconds):
        end = time.time() + seconds
        while time.time() < end:
            ready, _, _ = select.select([self.fd], [], [], 0.05)
            if not ready:
                continue
            try:
                data = os.read(self.fd, 65536)
            except OSError:
                return False
            if not data:
                return False
            self.log.write(data); self.log.flush()
            # Answer terminal capability queries a real terminal would answer.
            if b'\x1b[6n' in data:
                os.write(self.fd, b'\x1b[1;1R')
            if b'\x1b[c' in data or b'\x1b[0c' in data:
                os.write(self.fd, b'\x1b[?62;c')
        return True

    def screen(self):
        out = subprocess.run(['node', HERE + '/render.mjs', self.raw, str(COLUMNS), str(ROWS)], cwd=THIN, env=self.env.env,
                             capture_output=True, text=True, check=True)
        return out.stdout

    def wait_for(self, text, timeout=25):
        end = time.time() + timeout
        while time.time() < end:
            self.pump(0.4)
            if text in self.screen():
                return True
        return False

    def send(self, data):
        os.write(self.fd, data if isinstance(data, bytes) else data.encode())

    def typed(self, text, delay=0.02):
        for char in text:
            self.send(char); self.pump(delay)

    def close(self):
        try:
            os.kill(self.pid, signal.SIGTERM)
        except ProcessLookupError:
            pass
        self.pump(1)
        try:
            os.waitpid(self.pid, os.WNOHANG)
        except ChildProcessError:
            pass
        try:
            os.kill(self.pid, signal.SIGKILL)
            os.waitpid(self.pid, 0)
        except (ProcessLookupError, ChildProcessError):
            pass


