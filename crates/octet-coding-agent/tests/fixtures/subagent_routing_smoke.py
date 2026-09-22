"""Local-only smoke of real binary -> Python bundle -> Rust host -> alternate model."""
import http.server
import json
import os
import pathlib
import subprocess
import sys
import tempfile
import threading
ROOT = pathlib.Path(__file__).resolve().parents[4]
BINARY = pathlib.Path(sys.argv[1]).resolve()
requests = []
errors = []
parent_turn = 0
class Handler(http.server.BaseHTTPRequestHandler):
    def log_message(self, *_): pass
    def do_POST(self):
        global parent_turn
        body = json.loads(self.rfile.read(int(self.headers['Content-Length'])))
        requests.append(body)
        model = body['model']
        if model == 'alternate':
            delta = {'content': 'ALTERNATE_EXECUTED'}
            finish = 'stop'
        elif model == 'parent':
            parent_turn += 1
            tools = {t['function']['name'] for t in body.get('tools', [])}
            if parent_turn <= 3:
                name, args = [
                    ('subagent_models', {'query': 'provider', 'limit': 10}),
                    ('subagent_spawn', {'name': 'smoke-reader', 'task': 'Reply ALTERNATE_EXECUTED; do not call tools.', 'provider': 'custom/worker-provider', 'model': 'alternate', 'tools': ['read'], 'background': True}),
                    ('subagent_wait', {'target': 'smoke-reader', 'timeout_seconds': 10}),
                ][parent_turn - 1]
                if name not in tools:
                    errors.append('missing tool ' + name)
                if parent_turn > 1:
                    result = '\n'.join(str(m.get('content', '')) for m in body['messages'] if m.get('role') == 'tool')
                    if '"is_error":true' in result.replace(' ', '') or 'host_state_invalid' in result:
                        errors.append('error in prior tool result')
                delta = {'tool_calls': [{'index': 0, 'id': 'call_' + str(parent_turn), 'type': 'function', 'function': {'name': name, 'arguments': json.dumps(args)}}]}
                finish = 'tool_calls'
            else:
                delta = {'content': 'ROUTING_E2E_OK'}
                finish = 'stop'
        else:
            errors.append('unexpected wire model ' + model)
            delta, finish = {'content': 'UNEXPECTED_MODEL'}, 'stop'
        chunks = [
            {'id': 'mock', 'object': 'chat.completion.chunk', 'model': model, 'choices': [{'index': 0, 'delta': delta, 'finish_reason': None}]},
            {'id': 'mock', 'object': 'chat.completion.chunk', 'model': model, 'choices': [{'index': 0, 'delta': {}, 'finish_reason': finish}], 'usage': {'prompt_tokens': 20, 'completion_tokens': 5, 'total_tokens': 25}},
        ]
        payload = ''.join('data: ' + json.dumps(c) + '\n\n' for c in chunks).encode() + b'data: [DONE]\n\n'
        self.send_response(200); self.send_header('Content-Type', 'text/event-stream'); self.send_header('Content-Length', str(len(payload))); self.end_headers(); self.wfile.write(payload)

server = http.server.ThreadingHTTPServer(('127.0.0.1', 0), Handler)
thread = threading.Thread(target=server.serve_forever, daemon=True); thread.start()
try:
    with tempfile.TemporaryDirectory(prefix='octet-route-smoke-') as directory:
        home = pathlib.Path(directory)
        creds = home / '.octet/credentials'; creds.mkdir(parents=True, mode=0o700)
        store = creds / 'custom.json'
        store.write_text(json.dumps({
            'version': 1,
            'providers': {
                provider: {
                    'label': 'routing fixture',
                    'base_url': 'http://127.0.0.1:%d/v1/' % server.server_port,
                    'auth': {'kind': 'none'},
                    'auto_discover': False,
                    'models': [{
                        'api_name': model, 'context_window': 131072,
                        'max_output_tokens': output, 'tools': True, 'reasoning': False,
                    }],
                }
                for provider, model, output in [
                    ('parent-provider', 'parent', 4096),
                    ('worker-provider', 'alternate', 1024),
                ]
            },
        }))
        store.chmod(0o600)
        env = {'HOME': directory, 'PATH': os.environ['PATH'], 'TERM': 'dumb', 'NO_COLOR': '1'}
        command = [str(BINARY), '--offline', '--print', '--model', 'custom/parent-provider/parent', '--reasoning', 'off', '--max-turns', '8', '--workspace', directory, '--extension-dir', str(ROOT/'extensions'), '--enable-extension', 'octet-subagents', '--trust-extension', 'octet-subagents', '--', 'Run the deterministic local routing smoke.']
        result = subprocess.run(command, cwd=directory, env=env, capture_output=True, text=True, timeout=60)
        print('exit:', result.returncode)
        print('wire models:', [r['model'] for r in requests])
        print('stdout:', result.stdout[-2000:])
        print('stderr:', result.stderr[-2000:])
        assert result.returncode == 0, 'binary failed'
        assert not errors, errors
        assert any(r['model'] == 'alternate' for r in requests), 'alternate route never executed'
        assert 'ROUTING_E2E_OK' in result.stdout, 'parent did not finish'
        assert [r['model'] for r in requests].count('alternate') == 1
        alternate = next(r for r in requests if r['model'] == 'alternate')
        assert alternate.get('max_completion_tokens', alternate.get('max_tokens', 0)) == 1024
        assert parent_turn == 4, 'worker changed the parent conversation or route'
        parent_requests = [request for request in requests if request['model'] == 'parent']
        outputs = {
            message['tool_call_id']: message['content']
            for message in parent_requests[-1]['messages']
            if message.get('role') == 'tool'
        }
        discovery = json.loads(outputs['call_1'])
        assert discovery['truncated'] is False
        assert {row['model'] for row in discovery['models']} == {
            'custom/parent-provider/parent', 'custom/worker-provider/alternate',
        }
        assert 'custom/worker-provider/alternate' in outputs['call_2']
        assert 'applied by the host' in outputs['call_2']
        assert 'ALTERNATE_EXECUTED' in outputs['call_3']
        assert 'done' in outputs['call_3']
        # Inspect only our fixture-owned records, not real user sessions.
        records = '\n'.join(p.read_text() for p in home.rglob('*.jsonl'))
        assert 'ALTERNATE_EXECUTED' in records
        assert 'host_state_invalid' not in records
        assert 'custom/worker-provider/alternate' in records
        assert 'custom/parent-provider/parent' in records
        assert 'unsupported_model' not in records
        print('PASS: real binary/extension discovery, alternate spawn, wait, and parent completion')
finally:
    server.shutdown(); server.server_close()
