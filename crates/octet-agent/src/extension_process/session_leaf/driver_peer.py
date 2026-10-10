#!/usr/bin/env python3
# Routed session-repair.v2 peer: reads both descriptors through parent-bound
# chunk handles, then appends through the host-issued one-use grant.
import base64,hashlib,json,os,sys,traceback,time,threading
pending={}
responses={}
count=0
reads=0
def send(v): print(json.dumps(v,separators=(',',':')),flush=True)
# This entrypoint runs from a staged copy whose directory does not outlive
# startup, so knobs and records live in the real workspace (the host exports it).
workspace=os.environ.get('OCTET_WORKSPACE') or os.path.dirname(os.path.abspath(__file__))
def knob(name):
 try:
  with open(os.path.join(workspace,name)) as f: return f.read().strip()
 except OSError: return ''
def prepare_delay():
 # A loaded peer holds its preparation response open; the acceptance test sets
 # this knob so a publication backlog can accumulate deterministically. Absent
 # (or malformed) means no delay, exactly like the reviewed peer.
 try: ms=int(knob('prepare-delay-ms') or '0')
 except ValueError: return 0.0
 return max(0,min(5000,ms))/1000.0
def record_prepared(d):
 path=knob('prepare-record-path')
 if not path: return
 with open(path,'a') as f:
  f.write(json.dumps({'view_revision':d['view_revision'],'head':d.get('head'),'bytes':d['bytes']})+'\n')
def reply(m,r): send({'jsonrpc':'2.0','id':m['id'],'result':r})
def read_line():
 line=sys.stdin.readline()
 assert line,'peer eof'
 return json.loads(line)
def call(parent,method,params):
 global reads
 reads+=1; cid='child:%d'%reads
 send({'jsonrpc':'2.0','id':cid,'method':method,'params':dict(parent_request_id=parent,**params)})
 # Concurrent callers share one stdin; buffer responses that arrive early.
 if cid in responses:
  reply=responses.pop(cid); assert 'result' in reply,(method,reply); return reply['result']
 while True:
  if cid in responses:
   return responses.pop(cid)['result']
  m=read_line()
  if m.get('id')==cid and m.get('method') is None:
   assert 'result' in m,(method,m)
   return m['result']
  handle(m)
def read_document(parent,d,chunk_bytes=65536):
 data=bytearray(); offset=0
 while offset<d['bytes']:
  c=call(parent,'session/snapshot/read',{'transfer_id':d['transfer_id'],'offset':offset,'max_bytes':chunk_bytes})
  assert c['transfer_id']==d['transfer_id'] and c['offset']==offset,c
  raw=base64.b64decode(c['data'],validate=True)
  assert 0<len(raw)<=chunk_bytes
  data.extend(raw); offset=c['next_offset']
  assert c['eof']==(offset==d['bytes']),c
 assert offset==d['bytes'] and hashlib.sha256(bytes(data)).hexdigest()==d['sha256']
 r=call(parent,'session/snapshot/release',{'transfer_id':d['transfer_id']})
 assert r['released'] is True,r
 return json.loads(bytes(data))
def hydrated(m):
 p=m['params']; d=p.get('session_snapshot')
 if d is None: return p['payload']
 doc=read_document(m['id'],d)
 assert doc['head']==p['session_leaf']['expected_head'],doc['head']
 assert len(doc['entries'])==d['entry_count'] and len(doc['branch_ids'])==d['branch_count']
 payload=p.get('payload')
 if payload is None:
  inv=p.get('session_payload'); assert inv is not None,'payload descriptor'
  payload=read_document(m['id'],inv)
 if payload.get('preparation'):
  assert payload['preparation']['head']==doc['head']
 return payload
def do_hook(m):
 global count
 p=m['params']; payload=hydrated(m)
 if p.get('session_snapshot') is None:
  # Legacy mirror peers only; the paired profile must supply the descriptor.
  assert p['session_leaf']['expected_head']==p['context']['host']['session_leaf_id']
  assert len(p['context']['host']['session_entries'])==len(p['context']['host']['session_branch'])
 count+=1; cid='append:%d'%count; pending[cid]=('hook',m)
 send({'jsonrpc':'2.0','id':cid,'method':'session/append_entry','params':{'parent_request_id':m['id'],'entry_type':'driver-checkpoint','data':{'hook':p['hook'],'name':payload.get('name'),'count':count},'session_leaf':{k:p['session_leaf'][k] for k in ['grant_id','activation_epoch','operation_id']}}})
def handle(m):
 method=m.get('method')
 if method is None:
  assert 'result' in m,m
  if m['id']=='register-1':
   # Contribution responses are answered by the draining host, not the wire.
   return
  if m['id'] not in pending:
   assert isinstance(m['id'],str) and m['id'].startswith('child:'),m
   responses[m['id']]=m
   return
  kind,original=pending.pop(m['id'])
  if kind=='hook': reply(original,{'disposition':{'action':'continue'}})
  else: reply(original,{'content':[{'type':'text','text':'nested complete'}],'is_error':False})
 elif method=='session/snapshot/prepare':
  d=m['params']['snapshot']; delay=prepare_delay()
  if delay: time.sleep(delay)
  doc=read_document(m['id'],d)
  assert doc['head']==d['head'] and len(doc['entries'])==d['entry_count']
  record_prepared(d)
  reply(m,{'accepted':True,'transfer_id':d['transfer_id'],'view_revision':d['view_revision'],'head':d['head'],'sha256':d['sha256']})
 elif method=='hook/run': do_hook(m)
 elif method=='tool/call':
  # A published owner view may ride along on ordinary requests; do not consume it.
  cid='nested:%d'%m['id']; pending[cid]=('tool',m)
  send({'jsonrpc':'2.0','id':cid,'method':'composition/call','params':{'parent_request_id':m['id'],'name':'tiny','arguments':{}}})
 elif method=='probe':
  threading.Thread(target=lambda original=m: (time.sleep(0.75), reply(original,{})),daemon=True).start()
 elif method in ['context/updated','$/cancelRequest']: pass
 else: raise AssertionError(m)
def run():
 while True:
  m=read_line()
  if m.get('method')=='initialize':
   p=m['params']
   features=p['protocol']['required_features']+['session_entries','tool_composition_v1']
   # A reload candidate registers its contribution as soon as it is initialized,
   # which is before the host cuts the generation over.
   autocomplete=knob('autocomplete')!=''
   if autocomplete: features.append('autocomplete')
   reply(m,{'api_version':'0.4','tools':[{'name':'compose','description':'Compose','parameters':{'type':'object'},'composition':{'mode':'on','inline_budget':3000}}],'commands':[],
    'protocol':{'version':'0.4','features':features,'limits':{'max_concurrent_requests':4}}})
   if autocomplete:
    send({'jsonrpc':'2.0','id':'register-1','method':'ui/autocomplete/register','params':{'revision':1}})
  elif m.get('method')=='shutdown': reply(m,{}); break
  else: handle(m)
if __name__=='__main__':
 if knob('startup-stderr'):
  # A generation reports its startup state as soon as it is spawned, which is
  # before the host cuts a reload over.
  sys.stderr.write('candidate startup marker\n'); sys.stderr.flush()
 try: run()
 except BaseException:
  with open(os.path.join(os.path.dirname(os.path.abspath(__file__)),'peer-error.txt'),'w') as f: f.write(traceback.format_exc())
  raise
