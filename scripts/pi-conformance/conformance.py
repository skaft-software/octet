#!/usr/bin/env python3
"""Pi conformance harness.

Runs the same unchanged Pi examples through native Pi (its own interactive mode on a PTY) and
through octet with the Pi adapter. Both talk to the same kind of loopback mock provider; nothing
leaves the machine. For every scenario the user-visible frame (ANSI stripped, split into regions
above / editor / below) and the model request bodies the mock captured are reduced to named
observations, and the two columns are compared. A row passes only if current equals Pi.

usage: conformance.py [--out DIR] [--only name,name] [--all-examples] [--hosts pi,current]
Environment: E2E_OCTET (octet binary), E2E_NODE_BIN (Node 22.19.0 bin dir), E2E_PI_ROOT (see README.md).
Writes DIR/conformance.json and DIR/conformance.md.
"""
import argparse, hashlib, json, os, re, shutil, sys, tempfile, time

HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, HERE)
import drive  # noqa: E402

PI_DIR = drive.PI
PI_CLI = os.path.join(PI_DIR, 'dist/bundle/cli.js')
EXAMPLES = drive.EXAMPLES
NODE = os.path.join(drive.NODE_BIN, 'node')
ALL_HOSTS = ['pi', 'current']
HOSTS = list(ALL_HOSTS)
TMP = os.environ.get('E2E_TMP', tempfile.gettempdir())
RULE = re.compile(r'^[─\s]{20,}$')
BUILTIN_TOOLS = {'read', 'bash', 'edit', 'write', 'grep', 'find', 'ls', 'glob', 'powershell', 'exec', 'codemode', 'tool_search', 'todo_write'}


class PiEnv:
    """Native Pi: its own agent dir, models.json pointing at the loopback mock, explicit -e extensions."""
    exe = NODE

    def __init__(self, examples, provider_url, approve=True):
        self.approve = approve
        self.mode = 'pi'
        self.root = tempfile.mkdtemp(prefix='conf-pi-', dir=TMP)
        self.home = self.root + '/home'
        self.ws = self.root + '/ws'
        self.agent = self.root + '/agent'
        for d in (self.home, self.ws, self.agent):
            os.makedirs(d, exist_ok=True)
        os.makedirs(self.ws + '/.pi', exist_ok=True)
        open(self.ws + '/.pi/settings.json', 'w').write('{}')  # the octet harness workspace has it too; same project resources on both hosts
        json.dump({'providers': {'mock': {'baseUrl': provider_url.rstrip('/'), 'api': 'openai-completions', 'apiKey': 'x',
                   'models': [{'id': 'probe', 'name': 'probe', 'contextWindow': 32768, 'maxTokens': 4096}]}}}, open(self.agent + '/models.json', 'w'))
        json.dump({'defaultProvider': 'mock', 'defaultModel': 'probe', 'quietStartup': True, 'enableInstallTelemetry': False,
                   'collapseChangelog': True, 'lastChangelogVersion': '99.0.0'}, open(self.agent + '/settings.json', 'w'))
        self.examples = [x if os.path.isabs(x) else os.path.join(EXAMPLES, x) for x in examples]
        self.env = {'PATH': drive.NODE_BIN + ':/usr/bin:/bin', 'HOME': self.home, 'PWD': self.ws, 'TERM': 'xterm-256color', 'LANG': 'C.UTF-8',
                    'PI_OFFLINE': '1', 'PI_SKIP_VERSION_CHECK': '1', 'PI_CODING_AGENT_DIR': self.agent,
                    'COLUMNS': str(drive.COLUMNS), 'LINES': str(drive.ROWS)}

    def args(self, *extra):
        argv = ['node', PI_CLI, '--offline', '--no-context-files', '--no-skills', '--no-themes', '--no-prompt-templates', '--model', 'mock/probe', '--no-session'] + (['--approve'] if self.approve else [])  # --approve = octet's --workspace-trusted (trust override: no project_trust event)
        for path in self.examples:
            argv += ['-e', path]
        return argv + list(extra)

    def cleanup(self):
        shutil.rmtree(self.root, ignore_errors=True)


def make_env(host, examples, provider_url, approve=True):
    env = PiEnv(examples, provider_url, approve) if host == 'pi' else drive.Env(host, examples=examples, provider_url=provider_url)
    seed_agent_dir(env.agent)
    return env


# ---- migration safety: octet must never write to the Pi agent dir where Pi would not ---------------

def seed_agent_dir(agent):
    """Put the files a real user has in their Pi dir (credentials, a saved session) so changes to them are visible."""
    os.makedirs(agent + '/sessions/--seed--', exist_ok=True)
    open(agent + '/auth.json', 'w').write('{"seed-provider": {"type": "api_key", "key": "seed-not-a-secret"}}\n')
    os.chmod(agent + '/auth.json', 0o600)
    open(agent + '/sessions/--seed--/seed.jsonl', 'w').write('{"type":"session","version":3,"id":"seed","timestamp":"2026-01-01T00:00:00.000Z","cwd":"/seed"}\n')


def fingerprint(agent):
    """Every file's path and sha256 (symlinks by target, not followed) plus directories, relative to the agent dir."""
    out = {}
    for base, dirs, files in os.walk(agent, followlinks=False):
        for name in dirs + files:
            full = os.path.join(base, name)
            rel = os.path.relpath(full, agent)
            if os.path.islink(full):
                out[rel] = 'link:' + os.readlink(full)
            elif os.path.isdir(full):
                out[rel] = 'dir'
            else:
                out[rel] = hashlib.sha256(open(full, 'rb').read()).hexdigest()
    return out


def fp_delta(before, after):
    return {'added': sorted(set(after) - set(before)), 'deleted': sorted(set(before) - set(after)),
            'changed': sorted(k for k in before if k in after and before[k] != after[k])}


def touched(delta):
    return set(delta['added']) | set(delta['deleted']) | set(delta['changed'])


def safety(columns):
    """current may only touch what Pi touched for the same scenario; auth.json and sessions/ never change."""
    cur, pi = columns.get('current', {}).get('agent_dir'), columns.get('pi', {}).get('agent_dir')
    if cur is None or pi is None:
        return None
    extra = sorted(touched(cur) - touched(pi))
    protected = sorted(p for p in touched(cur) if p == 'auth.json' or p == 'sessions' or p.startswith('sessions/'))
    return {'extra_writes': extra, 'protected_changed': protected, 'ok': not extra and not protected, 'pi_touched': sorted(touched(pi)), 'current_touched': sorted(touched(cur))}


# ---- normalisation -------------------------------------------------------------------------------

def rows(screen):
    return [line.rstrip() for line in screen.split('\n')]


def regions(screen):
    r = rows(screen)
    rules = [i for i, line in enumerate(r) if RULE.match(line)]
    if len(rules) < 2:
        return {'above': r, 'editor': [], 'below': []}
    return {'above': r[:rules[0]], 'editor': r[rules[0] + 1:rules[-1]], 'below': r[rules[-1] + 1:]}


def squash(text):
    text = re.sub(r'/tmp/\S*?/(?:agent|home/\.pi/agent)\b', '<agent-dir>', text)  # per-run temp root of the Pi agent dir
    return re.sub(r'\s+', ' ', text).strip()


def has(screen, text, region=None):
    body = screen if region is None else '\n'.join(regions(screen)[region])
    return squash(text) in squash(body)


def notice_text(row):
    """Octet draws an extension notice as '• [Source] text'; Pi draws the bare text. Same text, host styling."""
    # Pi's notify(.., "error"|"warning") prints `Error: ` / `Warning: ` (interactive-mode showError/showWarning);
    # octet puts the level in the bracketed source instead.
    return re.sub(r'^(Error|Warning): ', '', re.sub(r'^•\s*(\[[^\]]+\]\s*)?', '', squash(row)))


def message_text(message):
    content = message.get('content')
    if isinstance(content, list):
        content = ' '.join(part.get('text', '') for part in content if isinstance(part, dict))
    return squash(content or '')


def request_view(mock, tool_names=()):
    """What the model received, reduced to what must match: extension tool definitions and the non-system conversation."""
    if not mock or not mock.requests:
        return None
    first, last = mock.requests[0], mock.requests[-1]
    tools = {t['function']['name']: t['function'] for t in (first.get('tools') or []) if t['function']['name'] in tool_names}
    convo = []
    for m in last.get('messages', []):
        if m.get('role') == 'system':
            continue
        if m.get('tool_calls'):
            convo.append(['assistant_tool_calls', [(c['function']['name'], json.loads(c['function']['arguments'] or '{}')) for c in m['tool_calls']]])
        else:
            convo.append([m.get('role'), message_text(m)])
    return {'requests': len(mock.requests), 'tools': {n: {'description': squash(f.get('description', '')), 'parameters': f.get('parameters')} for n, f in tools.items()},
            'conversation': convo}


# ---- driving -------------------------------------------------------------------------------------

def start(env):
    term = drive.Term(env)
    end = time.time() + 40
    while time.time() < end:
        term.pump(0.5)
        if len([line for line in rows(term.screen()) if RULE.match(line)]) >= 2:
            break
    term.pump(2.0)
    return term


def command(term, text, wait=None, timeout=15):
    term.typed(text); term.pump(0.5); term.send('\r')
    return term.wait_for(wait, timeout) if wait else (term.pump(2) or True)


def fixture_dir(name):
    """A copy of one Pi example directory whose npm dependencies are linked from the Pi install (no network):
    the same resolution `npm install` gives (`ms` 2.1.3 is the version pinned in with-deps/package.json; the
    Anthropic SDK is the one Pi itself ships)."""
    src = os.path.join(EXAMPLES, name)
    dest = os.path.join(TMP, 'pi-conformance-fixtures', name)
    if not os.path.isdir(dest + '/node_modules'):
        shutil.rmtree(dest, ignore_errors=True)
        os.makedirs(os.path.dirname(dest), exist_ok=True)
        shutil.copytree(src, dest)
        os.makedirs(dest + '/node_modules/@anthropic-ai', exist_ok=True)
        modules = os.path.join(drive.PI_ROOT, 'node_modules')
        for dep in ('ms', '@anthropic-ai/sdk'):
            if os.path.exists(os.path.join(modules, dep)) and not os.path.exists(os.path.join(dest, 'node_modules', dep)):
                os.symlink(os.path.join(modules, dep), os.path.join(dest, 'node_modules', dep))
    return dest + '/index.ts'


def ask(term, text, expect, timeout=30):
    term.typed(text); term.pump(0.5); term.send('\r')
    return term.wait_for(expect, timeout)


def settle(term, seconds=1.5):
    term.pump(seconds)
    return term.screen()


# ---- scenarios (host independent) ------------------------------------------------------------------

def s_hello(term, mock):
    obs = {'turn_finished': ask(term, 'say hello', 'all done')}
    screen = settle(term)
    obs['tool_result_in_transcript'] = has(screen, 'Hello, octet!', 'above')
    return obs, ['hello']


def s_commands(term, mock):
    obs = {'dialog_opened': command(term, '/commands', 'Available Commands')}
    screen = settle(term, 1)
    obs['lists_commands'] = has(screen, '/commands - List available slash commands')
    term.send('\x1b[B'); term.pump(0.5); term.send('\r')
    obs['source_prompt'] = term.wait_for('View source path?', 10)
    term.send('\r'); term.wait_for('commands.ts', 10); term.pump(0.5)
    screen = term.screen()
    obs['path_notified'] = 'examples/extensions/commands.ts' in screen.replace('\n', '').replace(' ', '')
    obs['dialog_closed'] = 'View source path?' not in screen and 'Available Commands' not in screen
    return obs, []


def s_status(term, mock):
    screen = settle(term)
    obs = {'ready_below_editor': has(screen, 'Ready', 'below')}
    ask(term, 'plain question', 'all done')
    screen = settle(term)
    obs['turn_complete_below_editor'] = has(screen, 'Turn 1 complete', 'below')
    return obs, []


def s_footer(term, mock):
    command(term, '/footer', 'footer', 8)
    screen = settle(term, 2)
    obs = {'cost_in_footer': has(screen, '$0.000', 'below')}
    command(term, '/footer'); screen = settle(term, 2)
    obs['custom_footer_removed'] = not has(screen, '↑0 ↓0 $0.000')
    return obs, []


def s_widget(term, mock):
    screen = settle(term)
    return {'above_widget_above_editor': has(screen, 'Above editor widget', 'above'),
            'below_widget_below_editor': has(screen, 'Below editor widget', 'below')}, []


def s_session_name(term, mock):
    obs = {'named': command(term, '/session-name e2e name', 'Session named: e2e name', 10)}
    command(term, '/session-name', 'Session: e2e name', 10)
    obs['readback'] = has(term.screen(), 'Session: e2e name')
    return obs, []


def s_header(term, mock):
    screen = settle(term)
    obs = {'mascot_shown': has(screen, 'shitty coding agent'), 'mascot_above_editor': has(screen, 'shitty coding agent', 'above')}
    command(term, '/builtin-header'); screen = settle(term, 2)
    obs['mascot_removed'] = not has(screen, 'shitty coding agent')
    return obs, []


def s_editor(term, mock):
    term.typed('ultrathink'); screen = settle(term, 1.5)
    raw = open(term.raw, 'rb').read()
    obs = {'text_in_editor': has(screen, 'ultrathink', 'editor'), 'per_character_truecolor': raw.count(b'38;2;') > 20}
    for _ in range(10):
        term.send('\x7f'); term.pump(0.05)
    obs['erased'] = not has(settle(term, 1), 'ultrathink')
    term.typed('submit through the custom editor'); term.pump(0.5); term.send('\r')
    obs['submitted_reaches_model'] = term.wait_for('all done', 30)
    return obs, []


def s_question(term, mock):
    term.typed('ask me'); term.pump(0.5); term.send('\r')
    obs = {'dialog_opened': term.wait_for('Pick one', 30)}
    screen = settle(term, 1)
    obs['options_listed'] = has(screen, 'Alpha') and has(screen, 'Beta')
    term.send('\r')
    obs['turn_finished'] = term.wait_for('all done', 30)
    return obs, ['question']


def s_overlay(term, mock):
    obs = {'overlay_shown': command(term, '/overlay-test', 'Overlay Test', 15)}
    obs['overlay_has_items'] = has(settle(term, 1), 'Settings')
    term.send('\x1b[B'); term.pump(0.4); term.send('\x1b[B'); term.pump(0.4); term.send('\r')
    term.pump(2)
    screen = term.screen()
    obs['result_notified'] = has(screen, 'Settings') and not has(screen, 'Overlay Test')
    return obs, []


def git_workspace(ws):
    import subprocess
    env = {'PATH': '/usr/bin:/bin', 'HOME': ws + '/..', 'GIT_CONFIG_GLOBAL': '/dev/null', 'GIT_CONFIG_SYSTEM': '/dev/null'}
    for argv in (['init', '-q', '-b', 'main'], ['config', 'user.name', 'conf'], ['config', 'user.email', 'conf@example.invalid'], ['config', 'commit.gpgsign', 'false']):
        subprocess.run(['git', *argv], cwd=ws, env=env, check=True)
    open(ws + '/seed.txt', 'w').write('seed\n')
    subprocess.run(['git', 'add', 'seed.txt'], cwd=ws, env=env, check=True)
    subprocess.run(['git', 'commit', '-q', '-m', 'seed'], cwd=ws, env=env, check=True)
    open(ws + '/dirty.txt', 'w').write('uncommitted\n')


def git_out(ws, *argv):
    import subprocess
    done = subprocess.run(['git', *argv], cwd=ws, env={'PATH': '/usr/bin:/bin', 'GIT_CONFIG_GLOBAL': '/dev/null', 'GIT_CONFIG_SYSTEM': '/dev/null'}, capture_output=True, text=True)
    return done.stdout.strip()


def s_auto_commit(term, mock):
    """session_shutdown commits the dirty tree with the last assistant text as the message."""
    ws = term.env.ws
    obs = {'turn_finished': ask(term, 'say hello', 'all done')}
    settle(term, 1)
    term.send('\x04'); term.pump(0.5); term.send('\x04')  # Ctrl+D on the empty editor: exit
    exited, end = False, time.time() + 20
    while time.time() < end and not exited:
        exited = not term.pump(0.5)
    obs['exited'] = exited
    obs['commit_subjects'] = git_out(ws, 'log', '--format=%s').split('\n')
    obs['tree_clean_after_exit'] = git_out(ws, 'status', '--porcelain') == ''
    return obs, []
s_auto_commit.setup = git_workspace


def s_with_deps(term, mock):
    screen = settle(term, 1)
    obs = {'turn_finished': ask(term, 'how long is two days', 'all done')}
    obs['tool_result_in_transcript'] = has(settle(term), '2 days = 172800000 milliseconds', 'above')
    return obs, ['parse_duration']


def s_rpc_demo(term, mock):
    screen = settle(term, 1)
    obs = {'widget_shown': has(screen, 'Loaded and ready.')}
    obs['input_dialog'] = command(term, '/rpc-input', 'Enter a value', 10)
    term.typed('forty-two'); term.pump(0.3); term.send('\r')
    obs['input_notified'] = term.wait_for('You entered: forty-two', 10)
    command(term, '/rpc-prefill', 'Editor prefilled', 10)
    obs['editor_prefilled'] = has(settle(term, 1), 'This text was set by the rpc-demo extension.', 'editor')
    return obs, []


def s_provider_registration(provider):
    def scenario(term, mock):
        settle(term, 1)
        obs = {'model_listed': command(term, '/model', provider, 15)}
        term.send('\x1b'); term.pump(0.5)
        return obs, []
    return scenario


def s_project_trust(term, mock):
    """project_trust fires on a workspace with project resources; the handler's select dialog is answered, the session carries on."""
    obs = {'dialog_opened': term.wait_for('Project trust for:', 20)}
    term.send('\r'); term.pump(3)  # "Trust and remember"; let the editor come up before typing
    obs['turn_finished'] = ask(term, 'plain question', 'all done', 30)
    return obs, []


s_project_trust.trust_prompt = True  # Pi without --approve resolves trust through the project_trust handlers


def mock_tool(tool, args):
    return lambda: drive.MockProvider(tool, args)


SCENARIOS = {
    'project-trust': (['project-trust.ts'], drive.MockProvider, s_project_trust),
    'auto-commit-on-exit': ([os.path.join(EXAMPLES, 'auto-commit-on-exit.ts')], drive.MockProvider, s_auto_commit),
    'with-deps': ([fixture_dir('with-deps')], lambda: drive.MockProvider('parse_duration', {'duration': '2 days'}), s_with_deps),
    'rpc-demo': (['rpc-demo.ts'], None, s_rpc_demo),
    'custom-provider-anthropic': ([fixture_dir('custom-provider-anthropic')], None, s_provider_registration('custom-anthropic')),
    'custom-provider-gitlab-duo': (['custom-provider-gitlab-duo/index.ts'], None, s_provider_registration('gitlab-duo')),
    'hello': (['hello.ts'], mock_tool('hello', {'name': 'octet'}), s_hello),
    'commands': (['commands.ts'], None, s_commands),
    'status-line': (['status-line.ts'], drive.MockProvider, s_status),
    'custom-footer': (['custom-footer.ts'], None, s_footer),
    'widget-placement': (['widget-placement.ts'], None, s_widget),
    'session-name': (['session-name.ts'], None, s_session_name),
    'custom-header': (['custom-header.ts'], None, s_header),
    'rainbow-editor': (['rainbow-editor.ts'], drive.MockProvider, s_editor),
    'question': (['question.ts'], mock_tool('question', {'question': 'Pick one', 'options': [{'label': 'Alpha'}, {'label': 'Beta'}]}), s_question),
    'overlay-test': (['overlay-test.ts'], None, s_overlay),
}


# Generic scenario for every other example: it loads, shows the same startup frame, and offers the
# model the same extension tools.
def s_generic(term, mock):
    screen = settle(term, 2.0)
    r = regions(screen)
    obs = {'startup_text': [notice_text(x) for x in r['above'] + r['below'] if squash(x) and not CHROME.search(x)],
           'load_error': next((squash(x)[:120] for x in rows(screen) if re.search(r'failed to load|extension error|Cannot find|ERR_|at registeredHandler|TypeError|ReferenceError', x, re.I)), '')}
    ask(term, 'plain question', 'all done', 20)
    return obs, None


# Examples that cannot run offline against a mock provider, with the reason.
SKIP = {
    'gondolin': 'imports @earendil-works/gondolin (not installed, no network) and needs a VM',
}
# Examples where octet differs from Pi for a reason outside the thin host (recorded, not hidden).
BLOCKED = {
    'preset.ts': 'configure writes invalid TOML (`default = undefined`) for a flag without a default, and --preset collides with an octet option',
    'shutdown-command.ts': 'Pi reports "Extension command /quit conflicts with built-in"; octet has no /quit built-in, so there is no such conflict to report (the command works)',
    'auto-commit-on-exit': 'pi.exec from session_shutdown is refused: the host ends the extension\'s process authority and stops serving host requests before it runs session_end (extensions/lifecycle.rs release_binding_for_transition); needs a host decision on shutdown-time exec',
    'jev-router.ts': 'pi.registerVirtualModel has no negotiated octet contract; configure refuses the extension',
    'custom-provider-anthropic': 'registerProvider config.oauth is refused by design (credential flow is host-owned), so configure refuses the extension',
    'custom-provider-gitlab-duo': 'registerProvider config.oauth is refused by design (credential flow is host-owned), so configure refuses the extension',
}
CHROME = re.compile(r'octet v|/changelog|permissions: |Ctrl\+D|[█▌]{3,}|/tmp/|probe|\$0|ctx|fd not found|^[─\s]+$')


def frame_diff(pi_rows, other_rows):
    clean = lambda rs: {r for r in rs if not CHROME.search(r)}
    a, b = clean(pi_rows), clean(other_rows)
    return {'only_in_pi': sorted(a - b)[:15], 'only_in_other': sorted(b - a)[:15]}


def example_path(name):
    return os.path.join(EXAMPLES, name)


def discover_examples():
    names = []
    for entry in sorted(os.listdir(EXAMPLES)):
        full = os.path.join(EXAMPLES, entry)
        if os.path.isdir(full):
            index = os.path.join(full, 'index.ts')
            if os.path.exists(index):
                names.append(entry + '/index.ts')
        elif entry.endswith('.ts'):
            names.append(entry)
    return names


def run_one(host, examples, mock_factory, fn):
    mock = (mock_factory or drive.MockProvider)()
    env = None
    result = {}
    try:
        env = make_env(host, examples, mock.url, not getattr(fn, 'trust_prompt', False))
        if getattr(fn, 'setup', None):
            fn.setup(env.ws)
        term = start(env)
        before = fingerprint(env.agent)
        try:
            obs, tools = fn(term, mock)
            screen = term.screen()
        finally:
            term.close()
        time.sleep(1.0)
        delta = fp_delta(before, fingerprint(env.agent))
        names = set(tools) if tools is not None else {t['function']['name'] for r in mock.requests[:1] for t in (r.get('tools') or [])} - BUILTIN_TOOLS
        result = {'obs': obs, 'model': request_view(mock, names), 'final_rows': [squash(x) for x in rows(screen) if squash(x)][:80], 'agent_dir': delta}
        if obs.get('load_error'):
            result['error'] = 'extension did not load: ' + obs['load_error']
    except (Exception, SystemExit) as error:  # a host that cannot run the scenario is a result, not a crash
        result = {'error': f'{type(error).__name__}: {str(error)[:300]}'}
    finally:
        mock.close()
        if env is not None:
            env.cleanup()
    return result


def compare(columns):
    """Row = one named observation. current passes only if it equals Pi."""
    keys = []
    for host in ALL_HOSTS:
        for part in ('obs', 'model'):
            value = columns.get(host, {}).get(part)
            if isinstance(value, dict):
                keys += [f'{part}.{k}' for k in value if f'{part}.{k}' not in keys]
    table = []
    for key in keys:
        part, name = key.split('.', 1)
        values = {h: (columns.get(h, {}).get(part) or {}).get(name, '<none>') for h in ALL_HOSTS}
        table.append({'key': key, 'values': values, 'current_matches_pi': values['current'] == values['pi']})
    return table


def fmt(value):
    text = json.dumps(value, ensure_ascii=False) if not isinstance(value, str) else value
    return text if len(text) <= 60 else text[:57] + '...'


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument('--out', default=TMP + '/conformance')
    ap.add_argument('--only')
    ap.add_argument('--all-examples', action='store_true', help='also run every other Pi example with a generic load/registration scenario')
    ap.add_argument('--skip-core', action='store_true', help='with --all-examples: only the generic ones')
    ap.add_argument('--hosts', default=','.join(ALL_HOSTS))
    args = ap.parse_args()
    global HOSTS
    HOSTS = args.hosts.split(',')
    os.makedirs(args.out, exist_ok=True)
    plan = {name: spec for name, spec in SCENARIOS.items() if not args.only or name in args.only.split(',')}
    if args.skip_core:
        plan = {}
    if args.all_examples:
        covered = {x for spec in SCENARIOS.values() for x in spec[0] if not os.path.isabs(x)} | {os.path.relpath(x, EXAMPLES) for spec in SCENARIOS.values() for x in spec[0] if x.startswith(EXAMPLES)} | {name + '/index.ts' for name in SCENARIOS}
        for name in discover_examples():
            if name not in covered and (not args.only or name in args.only.split(',')):
                plan[name] = ([name], None, s_generic)
    skipped = {name: SKIP[name.split('/')[0]] for name in list(plan) if name.split('/')[0] in SKIP}
    for name in skipped:
        plan.pop(name)
    results = {name: {'status': 'skipped', 'reason': reason} for name, reason in skipped.items()}
    for name, (examples, mock_factory, fn) in plan.items():
        columns = {}
        for host in HOSTS:
            columns[host] = run_one(host, examples, mock_factory, fn)
            print(f'{name:28} {host:8} load={os.getloadavg()[0]:.1f}', 'ERROR ' + columns[host]['error'][:100] if 'error' in columns[host] else 'ok', flush=True)
        pi = columns.get('pi', {})
        if 'error' in pi:
            results[name] = {'status': 'skipped', 'reason': 'native Pi cannot run it offline: ' + pi['error'][:200], 'columns': columns}
            continue
        table = compare(columns)
        diffs = {h: frame_diff(pi['final_rows'], columns[h].get('final_rows', [])) for h in ('current',) if 'final_rows' in columns.get(h, {})}
        blocked = BLOCKED.get(name) or BLOCKED.get(name.split('/')[0])
        safe = safety(columns)
        passed = bool(table) and (safe is None or safe['ok']) and all(row['current_matches_pi'] for row in table) and 'error' not in columns.get('current', {})
        results[name] = {'status': 'pass' if passed else ('blocked' if blocked else 'fail'), **({'reason': blocked} if blocked and not passed else {}), 'rows': table, 'agent_dir_safety': safe, 'frame_diff': diffs, 'columns': columns}
        json.dump(results, open(args.out + '/conformance.json', 'w'), indent=1, ensure_ascii=False)
    json.dump(results, open(args.out + '/conformance.json', 'w'), indent=1, ensure_ascii=False)
    lines = ['| example / observation | Pi | current | current = Pi |', '|---|---|---|---|']
    for name, res in results.items():
        if res['status'] == 'skipped':
            lines.append(f'| **{name}** | skipped | | {res["reason"][:90]} |')
            continue
        if not res.get('rows'):
            lines.append(f'| **{name}** | | | **{res["status"].upper()}** |')
        lines.append(f'| **{name}** | | | **{res["status"].upper()}** |')
        safe = res.get('agent_dir_safety')
        if safe is not None:
            lines.append(f'| agent dir writes (current beyond Pi, auth/sessions) | {fmt(safe["pi_touched"])} | {fmt(safe["current_touched"])} | {"yes" if safe["ok"] else "NO: " + fmt(safe["extra_writes"] + safe["protected_changed"])} |')
        for row in res['rows']:
            v = row['values']
            lines.append(f'| {row["key"]} | {fmt(v["pi"])} | {fmt(v["current"])} | {"yes" if row["current_matches_pi"] else "NO"} |')
    open(args.out + '/conformance.md', 'w').write('\n'.join(lines) + '\n')
    counts = {s: sum(1 for r in results.values() if r['status'] == s) for s in ('pass', 'fail', 'blocked', 'skipped')}
    print(json.dumps(counts), 'load', os.getloadavg()[0])
    return 1 if counts['fail'] else 0


if __name__ == '__main__':
    sys.exit(main())
