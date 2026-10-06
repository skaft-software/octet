#!/usr/bin/env python3
"""Handwritten adversarial API 0.4 peer, not an SDK parity fixture."""
import json, os, sys, threading, hashlib

transport = None
PAYLOAD = b'immutable-octet-bulk-v1' * 2048

write_lock = threading.Lock()
state_lock = threading.Lock()
responses = {}
barriers = {}
objects = {}
serial = 0

def send(value):
    with write_lock:
        print(json.dumps(value, separators=(',', ':')), flush=True)

def log(kind, **fields):
    with write_lock:
        with open('calls.jsonl', 'a') as f:
            f.write(json.dumps(dict(kind=kind, pid=os.getpid(), **fields)) + '\n')

def notice(kind, **fields):
    send(dict(jsonrpc='2.0', method='notification', params=dict(message=json.dumps(dict(kind=kind, **fields)))))

def reverse(parent, method, **fields):
    global serial
    with state_lock:
        serial += 1
        rid = 'child-' + str(serial)
        event = threading.Event()
        box = []
        responses[rid] = (event, box)
    send(dict(jsonrpc='2.0', id=rid, method=method, params=dict(parent_request_id=parent, **fields)))
    event.wait()
    return box[0]

class Circuit:
    def __init__(self):
        self.count = 0

def bulk_call(mid, name, args):
    payload = PAYLOAD * (2 if os.path.isfile('bulk-double-payload') else 1)
    def request(method, **params):
        response = reverse(mid, method, **params)
        if 'error' in response:
            raise ValueError(response['error'].get('data', {}).get('code', 'blob_unavailable'))
        return response['result']
    metadata = {}
    try:
        if name == 'bulk_create':
            data = {}
            if args.get('mixed'):
                ref = request('resource/register', type='demo.Circuit')
                objects[ref['$resource']] = Circuit()
                data['resource'] = ref
            ticket = request('bulk/write', profile=args.get('profile', 'local-file.v1'), capacity=args.get('capacity', len(payload)), media_type='application/octet-stream')
            scratch = open(os.path.join(transport['transfer_directory'], ticket['locator']), 'r+b')
            scratch.write(payload + (b'X' if args.get('oversize') else b''))
            scratch.flush()
            digest = dict(algorithm='sha256', value='0'*64 if args.get('wrong_digest') else hashlib.sha256(payload).hexdigest())
            blob = request('bulk/commit', ticket=ticket['ticket'], bytes=len(payload)+args.get('length_delta', 0), digest=digest)
            if args.get('rewrite'):
                scratch.seek(0)
                scratch.write(b'changed-original-after-host-snapshot')
                scratch.flush()
            scratch.close()
            data['blob'] = blob
            notice('output_ready', request=mid, resources=[data['resource']] if 'resource' in data else [], blobs=[blob])
            if args.get('diagnostic') or args.get('attachment_only'):
                metadata = dict(octet_diagnostics_v1=[dict(severity='info', code='bulk.fixture', message='bounded scientific summary', attachments=[dict(kind='blob', id=blob['$blob'])])])
            if args.get('attachment_only'):
                del data['blob']
            if args.get('bad_blob'):
                data['blob'] = dict(blob, **{'$blob':'fabricated'})
            if args.get('bad_resource'):
                data['resource'] = dict(data['resource'], **{'$resource':'fabricated'})
            if args.get('invalid'):
                data['unexpected'] = True
            if args.get('invalid_diagnostic'):
                metadata = dict(octet_diagnostics_v1=[dict(severity='nope', code='bad', message='bad')])
            if args.get('locator_leak'):
                data['leak'] = ticket['locator']
            return (None if args.get('error') else data), bool(args.get('error')), metadata
        if name == 'bulk_read':
            lease = request('bulk/read', profile=args.get('profile', 'local-file.v1'), blob=args['blob'])
            with open(os.path.join(transport['transfer_directory'], lease['locator']), 'rb') as f:
                data = f.read()
            notice('lease', request=mid, lease=lease['lease'])
            if not args.get('keep'):
                request('bulk/release', id=lease['lease'])
            return dict(bytes=len(data), verified=data == payload), False, {}
        if name == 'bulk_release':
            return request('bulk/release', id=args['id']), False, {}
    except ValueError as error:
        notice('bulk_error', request=mid, code=str(error))
        return None, True, {}
    raise AssertionError(name)

def call(message):
    mid = message['id']
    args = message['params']['arguments']
    name = message['params']['name']
    event = threading.Event()
    with state_lock:
        barriers[mid] = event
    log('call', name=name, request=mid)
    notice('entered', request=mid, name=name)
    data = {}
    error = False
    metadata = {}
    if name.startswith('bulk_'):
        data, error, metadata = bulk_call(mid, name, args)
    elif name == 'create':
        refs = []
        for _ in range(args.get('registrations', 1)):
            response = reverse(mid, 'resource/register', type='demo.Circuit')
            if 'error' in response:
                data = None
                error = True
                notice('registration_error', request=mid, error=response['error'])
                break
            ref = response['result']
            objects[ref['$resource']] = Circuit()
            refs.append(ref)
        if not error:
            data = dict(resource=refs[0])
            if len(refs) > 1:
                data['second'] = refs[1]
            notice('output_ready', request=mid, resources=refs)
        if args.get('invalid'):
            data['unexpected'] = True
        if args.get('error'):
            data = None
            error = True
    elif name == 'use':
        with state_lock:
            circuit = objects[args['resource']['$resource']]
            circuit.count += 1
            data = dict(count=circuit.count)
    elif name == 'release':
        result = reverse(mid, 'resource/release', resource=args['resource'])
        data = None
        error = 'error' in result
        notice('released', response=result)
    if args.get('block'):
        event.wait()
    if args.get('rpc_error'):
        send(dict(jsonrpc='2.0', id=mid, error=dict(code=-32001, message='domain failed')))
    else:
        text = 'failed' if error else 'done'
        if not error and isinstance(data, dict) and 'blob' in data:
            text = 'immutable blob: ' + json.dumps(data['blob'], separators=(',', ':'))
        result = dict(content=[dict(type='text', text=text)], is_error=error)
        if metadata:
            result['metadata'] = metadata
        if data is not None:
            result['structured_content'] = data
        send(dict(jsonrpc='2.0', id=mid, result=result))
    notice('terminal', request=mid)

log('process')
for line in sys.stdin:
    message = json.loads(line)
    method = message.get('method')
    if method is None:
        with state_lock:
            item = responses.pop(message['id'], None)
        if item:
            item[1].append(message)
            item[0].set()
    elif method == 'initialize':
        with open('catalog.json') as f:
            tools = json.load(f)
        protocol = message['params']['protocol']
        transport = protocol.get('bulk_objects_v1')
        features = json.load(open('features.json')) if os.path.exists('features.json') else ['resource_refs_v1', 'operation_descriptors_v1']
        send(dict(jsonrpc='2.0', id=message['id'], result=dict(api_version='0.4', tools=tools, protocol=dict(version='0.4', features=protocol['required_features'] + features, limits=protocol['limits']))))
    elif method == 'tool/call':
        threading.Thread(target=call, args=(message,), daemon=True).start()
    elif method == 'fixture/malformed':
        send({'jsonrpc': '2.0', 'id': message['params']['request']})
    elif method == 'fixture/allow':
        barriers[message['params']['request']].set()
    elif method == '$/cancelRequest':
        notice('cancelled', request=message['params']['id'])
    elif method == 'resource/dispose':
        mode = open('cleanup-mode').read() if os.path.exists('cleanup-mode') else 'completed'
        refs = message['params']['resources']
        log('dispose', resources=refs)
        for ref in refs:
            objects.pop(ref['$resource'], None)
        if mode != 'hang':
            send(dict(jsonrpc='2.0', id=message['id'], result=dict(results=[dict(resource=r, status=mode) for r in refs])))
            notice('disposed', resources=refs)
    elif method == 'shutdown':
        send(dict(jsonrpc='2.0', id=message['id'], result={}))
        sys.exit(0)
