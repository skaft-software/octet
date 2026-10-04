#!/usr/bin/env python3
"""Offline Pi interface/export inventory. Never imports/executes Pi or extension code.

Uses the already-installed jiti Babel parser, stopping in pre() before transforms.
No npm, network, builds, global modules, or writes outside the two owned docs.
--refresh updates structural/source evidence, preserving manual row annotations.
--check (default) detects reference, parser, adapter, inventory and Markdown drift.
"""
import argparse
from collections import Counter
import hashlib
import json
from pathlib import Path
import posixpath
import re
import subprocess
import sys

ROOT = Path(__file__).resolve().parents[1]
MATRIX = ROOT / 'docs/pi-api-coverage.json'
VIEW = ROOT / 'docs/pi-api-coverage.md'
PIN = '581e7ba78141a4d8b61cc9d11b8b22ae7e59195e'
DEFAULT_PI = '/Users/achumukundan/github/earendil-works/pi'
ADAPTER = 'extensions/octet-pi-compat/'
PARSER = ADAPTER + 'node_modules/jiti/dist/babel.cjs'
PACKAGES = ['coding-agent', 'tui', 'ai']
# Explicit contract roots, supplemented by every source reached through exports.
CONTRACTS = ['core/extensions/types.ts', 'core/session-manager.ts',
             'core/model-registry.ts', 'core/event-bus.ts', 'core/sdk.ts',
             'core/agent-session.ts', 'core/settings-manager.ts',
             'core/resource-loader.ts', 'core/footer-data-provider.ts']

NODE = r'''
const fs = require('node:fs');
const babel = require(process.argv[1]);
const inputs = JSON.parse(fs.readFileSync(0, 'utf8'));
function parse(path, source) {
  let ast;
  const result = babel({source, filename:path, ts:path.endsWith('.ts'),
    babel:{plugins:[()=>({pre(file){ast=file.ast; throw new Error('inventory-stop');}})]}});
  if (!ast) throw Error(path+': '+(result.error?.message || 'no AST'));
  return ast.program;
}
const out = {};
for (const [path, source] of Object.entries(inputs)) {
  const program = parse(path, source), rows=[], exports=[], imports=[], objects={};
  const text = n => n ? source.slice(n.start,n.end) : '';
  const name = n => n?.name ?? n?.value ?? text(n);
  const loc = n => ({file:path,line:n.loc.start.line});
  const add = (id,kind,n,extra={}) => rows.push({name:id,kind,...loc(n),...extra});
  function nested(n, prefix) {
    if (!n || typeof n !== 'object') return;
    if (n.type==='TSTypeLiteral') {
      for (const m of n.members) {
        const key = name(m.key) || '[call]';
        add(prefix+'.'+key,'nested-field',m);
        nested(m.typeAnnotation,prefix+'.'+key);
      }
      return;
    }
    for (const [key,v] of Object.entries(n)) {
      if (['loc','start','end','leadingComments','trailingComments'].includes(key)) continue;
      if (Array.isArray(v)) v.forEach(x=>nested(x,prefix));
      else if (v && typeof v === 'object') nested(v,prefix);
    }
  }
  function declaration(n) {
    if (!n) return;
    const id=name(n.id);
    if (['TSInterfaceDeclaration','ClassDeclaration','TSTypeAliasDeclaration'].includes(n.type)) {
      add(id,'declaration',n,{declaration_kind:n.type,extends:[...(n.extends||[]).map(x=>name(x.expression)), ...(n.superClass?[name(n.superClass)]:[])], signature: text(n.typeAnnotation)});
      const members=n.body?.body || n.typeAnnotation?.members || [];
      for (const m of members) {
        if (['private','protected'].includes(m.accessibility) || m.key?.type==='PrivateName') continue;
        let key=name(m.key) || '[call]';
        if (id==='ExtensionAPI' && key==='on') {
          const value=m.parameters?.[0]?.typeAnnotation?.typeAnnotation?.literal?.value;
          if (!value) throw Error('nonliteral ExtensionAPI.on event');
          key+='('+JSON.stringify(value)+')';
        }
        const member=id+'.'+(m.static?'static.':'')+key;
        add(member,key.startsWith('on(')?'event':'member',m,{signature:text(m).split('{')[0].trim()});
        nested(m.typeAnnotation,member+(m.type==='TSMethodSignature'?'.return':''));
        nested(m.returnType,member+'.return');
        for (const p of (m.parameters||m.params||[])) nested(p.typeAnnotation,member+'.'+name(p));
      }
      if(n.type==='TSTypeAliasDeclaration' && !n.typeAnnotation?.members) nested(n.typeAnnotation,id);
    }
  }
  // Keep exported declarations and private types actually referenced by their
  // signatures. Do not turn unrelated implementation classes into public API.
  const declarations=new Map(), reachable=new Set();
  for(const statement of program.body) {
    const n=statement.declaration || statement;
    if(n.id?.name) declarations.set(n.id.name,n);
    if(statement.type==='ExportNamedDeclaration') {
      if(n.id?.name) reachable.add(n.id.name);
      for(const s of statement.specifiers) if(s.local?.name) reachable.add(s.local.name);
    }
  }
  function referenced(n) {
    if(!n || typeof n!=='object' || n.type==='BlockStatement')return;
    if(n.type==='TSTypeReference') reachable.add(text(n.typeName).split('.')[0]);
    if(n.type==='TSExpressionWithTypeArguments')reachable.add(text(n.expression).split('.')[0]);
    for(const [k,v] of Object.entries(n)) {
      if(['loc','start','end','leadingComments','trailingComments'].includes(k))continue;
      if(Array.isArray(v))v.forEach(referenced);else if(v&&typeof v==='object')referenced(v);
    }
  }
  for(let size=-1;size!==reachable.size;) {
    size=reachable.size; for(const id of [...reachable]) referenced(declarations.get(id));
  }
  for (const n of program.body) {
    if (n.type==='ExportAllDeclaration') exports.push({name:'*',source:n.source.value,kind:n.exportKind,...loc(n)});
    else if(n.type==='ExportNamedDeclaration') {
      declaration(n.declaration);
      if(n.declaration) {
        const d=n.declaration;
        const names=d.declarations ? d.declarations.flatMap(x=>x.id.type==='Identifier'?[x.id.name]:[]) : [name(d.id)];
        for (const key of names) exports.push({name:key,kind:n.exportKind||'value',...loc(d),snippet:text(d)});
      }
      for (const s of n.specifiers) exports.push({name:name(s.exported),local:name(s.local),source:n.source?.value,kind:n.exportKind==='type'?'type':s.exportKind||'value',...loc(s)});
    } else if(reachable.has(n.id?.name)) declaration(n);
    if(n.type==='ImportDeclaration') imports.push({source:n.source.value,names:n.specifiers.map(x=>name(x.imported)||'*'),kind:n.importKind,...loc(n)});
  }
  // Extract adapter facade object keys with actual locations, not name searches.
  function walk(n, parent) {
    if(!n || typeof n!=='object') return;
    if(n.type==='ObjectExpression') {
      let key=parent?.type==='VariableDeclarator'?name(parent.id):null;
      if(parent?.type==='CallExpression' && parent.callee?.name==='strict') key=parent.arguments[1]?.value;
      if(key) objects[key]=n.properties.filter(x=>x.key).map(x=>({name:name(x.key),...loc(x),snippet:text(x)}));
    }
    for(const [k,v] of Object.entries(n)) {
      if(['loc','start','end','leadingComments','trailingComments'].includes(k))continue;
      if(Array.isArray(v)) v.forEach(x=>walk(x,n)); else if(v&&typeof v==='object')walk(v,n);
    }
  }
  if(path.endsWith('.mjs'))walk(program);
  out[path]={rows,exports,imports,objects};
}
process.stdout.write(JSON.stringify(out));
'''


def digest(data):
    return hashlib.sha256(data).hexdigest()


def git(repo, *args):
    return subprocess.check_output(['git', '-C', str(repo), *args], timeout=30)


def ast(sources):
    result = subprocess.run(['node', '-e', NODE, str(ROOT / PARSER)],
                            input=json.dumps(sources), text=True, capture_output=True, timeout=120)
    if result.returncode:
        raise RuntimeError(result.stderr)
    return json.loads(result.stdout)


def reference(repo):
    files = set(git(repo, 'ls-tree', '-r', '--name-only', PIN).decode().splitlines())
    sources, manifests, roots, paths = {}, {}, {}, []

    def load(path):
        if path not in sources:
            sources[path] = git(repo, 'show', f'{PIN}:{path}').decode()
        return sources[path]

    def resolve(base, target):
        if not target.startswith('.'):
            return None
        path = posixpath.normpath(posixpath.join(posixpath.dirname(base), target))
        if path.endswith('.js'):
            path = path[:-3] + '.ts'
        for candidate in [path, path + '.ts', path + '/index.ts']:
            if candidate in files:
                return candidate
        raise ValueError(f'unresolved relative export: {base} -> {target}')

    for package in PACKAGES:
        path = f'packages/{package}/package.json'
        manifests[path] = json.loads(load(path))
        roots[package] = f'packages/{package}/src/index.ts'
        for key, value in manifests[path].get('exports', {'.': manifests[path]['main']}).items():
            target = value if isinstance(value, str) else value.get('source', value.get('import', value.get('default')))
            paths.append({'package':package,'path':key,'target':target,'file':path,'line':1})
            if not target:
                continue
            source = target.replace('./dist/', f'packages/{package}/src/').replace('./src/', f'packages/{package}/src/')
            source = re.sub(r'\.js$', '.ts', source)
            if '*' in source:
                pattern = re.compile('^' + re.escape(source).replace(r'\*', '(.*)') + '$')
                for file in sorted(files):
                    match = pattern.match(file)
                    if match:
                        paths.append({'package':package,'path':key.replace('*',match[1]),'target':file,'file':path,'line':1})
                        load(file)
            elif source in files:
                load(source)
                paths[-1]['source'] = source
        load(roots[package])
    for file in CONTRACTS:
        load('packages/coding-agent/src/' + file)
    # AgentToolResult, callback and agent tool definitions are imported contracts.
    load('packages/agent/src/types.ts')
    # Enumerate original pinned example imports as a distinct source corpus.
    for file in sorted(files):
        if file.startswith('packages/coding-agent/examples/extensions/') and file.endswith('.ts'):
            load(file)
    parsed = {}
    while True:
        batch = {p:s for p,s in sources.items() if p.endswith('.ts') and p not in parsed}
        if not batch:
            break
        parsed.update(ast(batch))
        for p in batch:
            for export in parsed[p]['exports']:
                if export.get('source'):
                    target = resolve(p, export['source'])
                    if target:
                        load(target)
    # Public import graph: imports are evidence only, never executed.
    return sources, parsed, manifests, roots, paths, resolve


def build(repo):
    sources, parsed, manifests, roots, paths, resolve = reference(repo)
    candidate_paths = sorted(set([PARSER, ADAPTER+'package.json', ADAPTER+'package-lock.json', ADAPTER+'child-sdk/package.json', ADAPTER+'child-sdk/dist/cli.js'] +
        [str(p.relative_to(ROOT)) for d in ['lib','shims','child-sdk','test'] for p in (ROOT/ADAPTER/d).glob('*.mjs')]))
    candidates = {p:(ROOT/p).read_text() for p in candidate_paths if p.endswith('.mjs')}
    local = ast(candidates)
    api = local[ADAPTER+'lib/api.mjs']['objects']
    rows = {}

    def insert(id, category, source, **extra):
        if id in rows:
            if source not in rows[id]['reference']:
                rows[id]['reference'].append(source)
            return
        rows[id] = {'id':id,'category':category,'reference':[source],
                    'implementation':{'status':'unverified','evidence':[]},
                    'reachability':{'status':'unverified','evidence':[]},
                    'native_tests':{'status':'unverified','evidence':[]},
                    'original_extension_acceptance':{'status':'unverified','evidence':[]},
                    'synthetic_tests':{'status':'unverified','evidence':[]},
                    'gap':'Per-member semantic/host/original-extension qualification unverified.', **extra}

    for file, info in parsed.items():
        for r in info['rows']:
            insert(file+'#'+r['name'], r['kind'], {'file':file,'line':r['line']},
                   name=r['name'],signature=r.get('signature',''),extends=r.get('extends',[]))
    # ReadonlySessionManager is a Pick, not an interface: enumerate every literal.
    session = 'packages/coding-agent/src/core/session-manager.ts'
    readonly = next(r for r in parsed[session]['rows'] if r['name']=='ReadonlySessionManager')
    for member in re.findall(r'"([^"]+)"', readonly['signature']):
        insert(session+'#ReadonlySessionManager.'+member,'member',{'file':session,'line':readonly['line']},name='ReadonlySessionManager.'+member)
    # Flatten interface inheritance where the parent is an enumerated interface.
    types = {r['name']:r for r in list(rows.values()) if r['category']=='declaration'}
    for _ in range(6):
        before = len(rows)
        for name, r in types.items():
            for base in r['extends']:
                for p in list(rows.values()):
                    if p.get('name','').startswith(base+'.') and p['category'] in ['member','nested-field']:
                        suffix=p['name'][len(base):]
                        file=r['reference'][0]['file']
                        insert(file+'#'+name+suffix,p['category'],p['reference'][0],name=name+suffix,inherited_from=base)
        if before==len(rows): break

    cache = {}
    def exports(file, visiting=()):
        if file in cache: return cache[file]
        if file in visiting: return {}
        output = {}
        for e in parsed[file]['exports']:
            if e['name']=='*':
                target=resolve(file,e['source'])
                if target:
                    for name, origin in exports(target,visiting+(file,)).items():
                        if name!='default': output.setdefault(name,origin)
                else:
                    # External stars cannot be silently counted as complete.
                    output['* from '+e['source']]=e
            else: output[e['name']]=e
        cache[file]=output
        return output
    for package, root in roots.items():
        for namespace in ['@earendil-works','@mariozechner']:
            for symbol, origin in exports(root).items():
                insert(f'{namespace}/pi-{package}#{symbol}','export',{'file':origin['file'],'line':origin['line']},
                       name=symbol,package=package,export_kind=origin['kind'],alias=namespace)
    for p in paths:
        for namespace in ['@earendil-works','@mariozechner']:
            path=namespace+'/pi-'+p['package']+('' if p['path']=='.' else p['path'][1:])
            insert(path,'import-path',{'file':p['file'],'line':1},name=path,target=p['target'])
            target=p.get('source',p['target'])
            if target and target in parsed:
                for symbol, origin in exports(target).items():
                    insert(path+'#'+symbol,'subpath-export',{'file':origin['file'],'line':origin['line']},
                           name=symbol,package=p['package'],export_kind=origin['kind'])
    for path in ['child-sdk/index.mjs','child-sdk/package.json','child-sdk/dist/cli.js','shims/child-sdk.mjs']:
        insert('octet-pi-compat/'+path,'child-path',{'file':ADAPTER+path,'line':1},name=path)
        if ADAPTER+path in local:
            for export in local[ADAPTER+path]['exports']:
                if export['name'] != '*':
                    insert('octet-pi-compat/'+path+'#'+export['name'],'child-export',
                           {'file':export['file'],'line':export['line']},name=export['name'])
    for package in PACKAGES:
        manifest=manifests[f'packages/{package}/package.json']
        for name,target in manifest.get('bin',{}).items():
            insert('@earendil-works/pi-'+package+'/'+target,'cli-path',
                   {'file':f'packages/{package}/package.json','line':1},name=name,target=target)
    for file,info in {**parsed,**local}.items():
        for imp in info['imports']:
            if re.match(r'@(?:earendil-works|mariozechner)/pi-(?:coding-agent|ai|tui)(?:/|$)',imp['source']):
                for symbol in imp['names']:
                    insert('import:'+imp['source']+'#'+symbol,'observed-import',
                           {'file':file,'line':imp['line']},name=symbol,import_path=imp['source'],export_kind=imp['kind'])
    # Separate context registry (available via ctx) from refused SDK constructor.
    registry='packages/coding-agent/src/core/model-registry.ts'
    for r in parsed[registry]['rows']:
        if r['name'].startswith('ModelRegistry.'):
            insert('ctx.'+r['name'],'context-registry',{'file':registry,'line':r['line']},name='ctx.'+r['name'])

    corpus_metadata={}
    corpus_path=ROOT/'artifacts/takeover/originals/corpus-ast.json'
    if corpus_path.exists():
        corpus_bytes=corpus_path.read_bytes()
        corpus=json.loads(corpus_bytes)
        corpus_metadata={'artifact':str(corpus_path.relative_to(ROOT)),'sha256':digest(corpus_bytes),
                         'scope':corpus['scope'],'parser':corpus['parser'],'packages':[]}
        for package in corpus['packages']:
            package_name=package['name']
            corpus_metadata['packages'].append({k:package[k] for k in ['name','version','root','source_hash','source_hash_algorithm','files','peerDependencies','parse_diagnostics'] if k in package})
            def origin(item):
                relative=posixpath.relpath(item['path'],package['root'])
                return {'file':'corpus/'+package_name+'/'+relative,'line':item['line'],
                        'sha256':package['files'].get(relative),'artifact':str(corpus_path.relative_to(ROOT))}
            profile_id='corpus:'+package_name+'#version-profile'
            insert(profile_id,'corpus-profile',{'file':str(corpus_path.relative_to(ROOT)),'line':1},name=package_name,
                   peers=package.get('peerDependencies',{}),source_hash=package['source_hash'])
            rows[profile_id]['gap']='Exact recorded peer ranges require profile qualification; installed package version is not proof of Pi 1.0 compatibility.'
            if package_name=='pi-background-tasks':
                rows[profile_id]['implementation']={'status':'version-profile-incompatible','evidence':[str(corpus_path.relative_to(ROOT))]}
                rows[profile_id]['gap']='Pi peer ranges ^0.81.1 || ^0.82.1 || ^0.83.0 || ^0.84.0 exclude pinned Pi 1.0.0. Do not silently widen the qualification profile.'
            imports={}
            for index,item in enumerate(package['imports']):
                if re.match(r'@(?:earendil-works|mariozechner)/pi-',item['specifier']):
                    if re.match(r'\s*(?:import|export)\s',item['text']):imports[f'corpus-import-{index}.ts']=item['text']
            imports_ast=ast(imports) if imports else {}
            for index,item in enumerate(package['imports']):
                if not re.match(r'@(?:earendil-works|mariozechner)/pi-',item['specifier']):continue
                found=imports_ast.get(f'corpus-import-{index}.ts',{})
                names=[n for imp in found.get('imports',[]) for n in imp['names']]
                names += [e['name'] for e in found.get('exports',[])]
                for symbol in names or ['[module-resolution]']:
                    insert('corpus:'+package_name+'#'+item['specifier']+'#'+symbol,'corpus-import',origin(item),
                           name=symbol,import_path=item['specifier'],type_only=item['type_only'])
            for item in package['calls']:
                if not item['candidates'] and not re.match(r'(?:pi|ctx)\.',item['callee']):continue
                name=item['callee']
                if item['arguments'] and re.search(r'(?:\.on|require|import|resolve)$',name):
                    first=item['arguments'][0]
                    if len(first)<=256:name+='('+first+')'
                insert('corpus:'+package_name+'#call:'+name,'corpus-call-candidate',origin(item),name=name,
                       candidate_kinds=item['candidates'])
            for item in package['assignments']:
                if not re.search(r'prototype|beforeToolCall|\._|editor|Editor|layout|Layout',item['target']):continue
                insert('corpus:'+package_name+'#assignment:'+item['target'],'corpus-private-assignment',origin(item),name=item['target'])
            completeness='corpus:'+package_name+'#dynamic-private-resolution'
            insert(completeness,'inventory-gap',{'file':str(corpus_path.relative_to(ROOT)),'line':1},name=package_name)
            rows[completeness]['gap']='Open inventory gate: AST candidate labels and syntactic pi/ctx roots do not resolve aliases, runtime-computed imports, transitive private monkey patches or execution reachability. Review original source scenarios; no exhaustive semantic corpus claim.'

    object_for={'ExtensionAPI':'pi','ExtensionUIContext':'ctx.ui','ExtensionContext':'ctx',
                'ExtensionToolContext':'ctx','ExtensionCommandContext':'ctx','ReplacedSessionContext':'ctx',
                'ReadonlySessionManager':'ctx.sessionManager','ctx.ModelRegistry':'ctx.modelRegistry',
                'ReadonlyFooterDataProvider':'footerData','EventBus':'pi.events'}
    shim_exports={k:{e['name']:e for e in local[ADAPTER+'shims/'+k+'.mjs']['exports']} for k in ['coding-agent','tui','ai']}
    api.update(local[ADAPTER+'lib/runtime.mjs']['objects'])
    test_tokens = {}
    for p in sorted((ROOT/ADAPTER/'test').glob('*.test.mjs')):
        seen=set()
        for number,line in enumerate(p.read_text().splitlines(),1):
            for token in set(re.findall(r'\b[A-Za-z_][A-Za-z_0-9]*\b',line)) - seen:
                test_tokens.setdefault(token,[]).append(f'{p.relative_to(ROOT)}:{number}')
                seen.add(token)
    for r in rows.values():
        name=r['name']; impl=None
        group, _, member=name.rpartition('.')
        if group in object_for:
            obj=object_for[group]
            impl=next((v for v in api.get(obj,[]) if v['name']==member),None)
            if impl:
                refused='unsupported(' in impl['snippet'] and not any(x in impl['snippet'] for x in ['runtime.','return op(','current().','operation(','return custom('])
                r['implementation']={'status':'explicit-refusal' if refused else 'source-present-partial','evidence':[f"{impl['file']}:{impl['line']}"]}
                r['reachability']={'status':'refused' if refused else 'unverified; facade entry exists, host/feature/owner admission required','evidence':r['implementation']['evidence']}
                r['gap']='Explicit refusal is an open compatibility gap.' if refused else 'Source presence is not full signature/semantics or actual-host qualification.'
            else:
                r['implementation']={'status':'missing-facade-member','evidence':[ADAPTER+'lib/api.mjs:1',ADAPTER+'lib/errors.mjs:1']}
                r['reachability']={'status':'refused-by-strict-facade','evidence':[ADAPTER+'lib/errors.mjs:1']}
                r['gap']='Pinned member absent from current strict facade.'
        if r['category']=='event':
            event=name[len('ExtensionAPI.on('):-1].strip('"')
            match=next((v for v in api.get('hookEvents',[]) if v['name']==event),None)
            match=match or next((v for v in api.get('notificationEvents',[]) if re.search(r"['\"]"+re.escape(event)+r"['\"]",v['snippet'])),None)
            r['implementation']={'status':'mapped-partial' if match else 'unsupported-event','evidence':[f"{match['file']}:{match['line']}" if match else ADAPTER+'lib/api.mjs:91']}
            r['reachability']={'status':'unverified; negotiated dispatch required' if match else 'registration-refused','evidence':r['implementation']['evidence']}
            r['gap']='Event payload/result fields require independent rows and qualification; mapping is not equivalence.' if match else 'No current hook/notification mapping.'
            member=event
        if r['category']=='export':
            match=shim_exports[r['package']].get(name)
            if r['export_kind']=='type':
                status='type-only-reference'; reach='erased import; runtime and authoring type parity unverified'
            else:
                status='shim-export-present-unverified' if match else 'missing-shim-export'
                reach='unverified; may explicitly refuse' if match else 'not-exported-by-facade'
                if match and ('unsupported(' in match.get('snippet','') or
                              (r['package']=='ai' and name in ['completeSimple','streamSimple'])):
                    status='explicit-refusal'; reach='refused'
            r['implementation']={'status':status,'evidence':[f"{match['file']}:{match['line']}" if match else ADAPTER+'shims/'+r['package']+'.mjs:1']}
            r['reachability']={'status':reach,'evidence':[ADAPTER+'lib/runtime.mjs:158']}
        if r['category'] in ['import-path','subpath-export']:
            r['gap']='Only package-root aliases are configured; subpath/conditional/wildcard resolution unverified, never inferred from root alias.'
        if r['category'] in ['child-path','child-export','cli-path']:
            r['gap']='Host child facade is not authorization. General Pi product admission, nested/custom callbacks, CLI argv/events and Pi session-file parity remain unverified or refused.'
        if group in ['SettingsManager','SettingsManager.static','DefaultResourceLoader','ModelRegistry']:
            r['implementation']={'status':'constructor-or-static-refusal; exact member parity unverified','evidence':[ADAPTER+'shims/child-sdk.mjs:9' if group!='ModelRegistry' else ADAPTER+'shims/coding-agent.mjs:27']}
            r['gap']='This SDK object cannot be constructed through its current facade. Pure methods are not automatically classified as child-product-gated.'
        if name.startswith('ExtensionAPI.sendMessage'):
            r['gap']='Custom transcript display and deliverAs steer/followUp/nextTurn are not implemented; triggerTurn uses a separate user-message injection, not proven Pi custom-message delivery semantics.'
            if name.endswith('.options.deliverAs'):
                r['implementation']={'status':'explicit-refusal','evidence':[ADAPTER+'lib/api.mjs:120']}
                r['reachability']={'status':'unknown-option-rejected','evidence':[ADAPTER+'lib/api.mjs:120']}
        if name.startswith('ExtensionAPI.sendUserMessage'):
            r['gap']='Options (deliverAs and expandPromptTemplates) are explicitly refused; media/array content parity unverified.'
            if '.options.' in name:
                r['implementation']={'status':'explicit-refusal','evidence':[ADAPTER+'lib/api.mjs:116']}
                r['reachability']={'status':'refused','evidence':[ADAPTER+'lib/api.mjs:116']}
        if group=='ToolDefinition' and r['category']=='member':
            allowed={'name','label','description','promptSnippet','promptGuidelines','parameters','execute','renderCall','renderResult','output_schema'}
            r['implementation']={'status':'registration-field-accepted-semantics-unverified' if member in allowed else 'registration-field-rejected','evidence':[ADAPTER+'lib/api.mjs:54']}
            if member in ['renderCall','renderResult']:
                r['gap']='Callback accepted at registration; invocation/native transcript rendering unverified, not established by field acceptance.'
        token=member or name
        if len(token)>3 and re.fullmatch(r'[A-Za-z_][A-Za-z_0-9]*',token):
            hits=test_tokens.get(token,[])
            if hits:r['synthetic_tests']={'status':'source-candidates-only; not run or semantically qualified','evidence':hits[:4]}
    return {'schema_version':1,'status':'pinned structural inventory; NOT compatibility qualification',
            'reference':{'repository':DEFAULT_PI,'commit':PIN,'checkout_head_observed_not_oracle':git(repo,'rev-parse','HEAD').decode().strip(),'versions':{p:manifests[f'packages/{p}/package.json']['version'] for p in PACKAGES},
                         'files':{p:digest(s.encode()) for p,s in sorted(sources.items())}},
            'candidate_files':{p:digest((ROOT/p).read_bytes()) for p in candidate_paths},
            'original_corpus':corpus_metadata,
            'method':'Babel AST exported declaration/member/event enumeration plus local types referenced by exported signatures, inheritance expansion, ReadonlySessionManager Pick literals, recursive named/star exports and package export-map wildcard expansion. Unrelated unexported top-level declarations excluded. No Pi code executed.',
            'limitations':['Generated native/original defaults are unverified; audited per-row annotations can cite current evidence. Historical aggregate receipts are not per-member acceptance.',
                           'Synthetic test pointers are lexical navigation candidates, not claims that a scenario is asserted or passed.',
                           'Local Pi checkout HEAD is deliberately ignored; immutable git objects supply every reference file.',
                           'Pinned reference is Pi 1.0.0; adapter selected TUI dependency is 0.85.0, not silently promoted to 1.0.',
                           'Package export map is inventoried, not proof that unpublished source/conditional/subpath exports resolve through jiti.',
                           'Private/protected class members excluded; public underscore methods retained. Parameter/return object fields enumerated; arbitrary dynamic/private imports and external-package export stars need separate corpus evidence.',
                           'Pinned upstream example imports plus original_corpus static import symbols and SDK/CLI/private-patch candidates are enumerated. Each original package has an explicit inventory-gap row for unresolved alias/dynamic/private semantic reachability.',
                           'Source statuses are conservative; source-present-partial does not establish read-after-write, lifecycle, payload, or frontend semantics.',
                           'Docs/design historical gaps are navigation only; this inventory uses current candidate source and the test-matching pin.'],
            'rows':sorted(rows.values(),key=lambda r:r['id'])}


AXES = ['implementation','reachability','native_tests','original_extension_acceptance','synthetic_tests']


def effective(row):
    result=dict(row)
    annotations=row.get('annotations',{})
    for key in AXES+['gap']:
        if key in annotations:
            if key in AXES and (not isinstance(annotations[key],dict) or
                                not isinstance(annotations[key].get('status'),str) or
                                not isinstance(annotations[key].get('evidence'),list)):
                raise ValueError(f"{row['id']}: annotation {key} needs status and evidence")
            result[key]=annotations[key]
    return result


def render(data):
    out=['# Pinned Pi API/path coverage','',
         '<!-- Generated by scripts/check-pi-api-coverage.py --render; edit JSON annotations, not this view. -->','',
         '**Not a compatibility pass.** The JSON is authoritative. Native/original qualification requires exact audited per-row evidence; generated defaults never assert it.','',
         f"Reference: Pi **{data['reference']['versions']['coding-agent']}**, commit `{PIN}`. File SHA-256 pins are in `pi-api-coverage.json`.",
         'Both namespace spellings have separate export/path rows. Adapter TUI remains **0.85.0**.',
         f"Observed checkout HEAD `{data['reference']['checkout_head_observed_not_oracle']}` is **not** the oracle.",'',
         '## Reproduce (offline)','', '```sh',
         'python3 -B scripts/check-pi-api-coverage.py --check --pi-repo /absolute/local/pi',
         'python3 -B scripts/check-pi-api-coverage.py --refresh --pi-repo /absolute/local/pi',
         'python3 -B scripts/check-pi-api-coverage.py --render',
         'python3 -B scripts/check-pi-api-coverage.py --self-test', '```','',
         'Requires Node and the candidate adapter\'s already-installed pinned jiti parser. Does not install anything, execute extension/Pi code, build Rust, access the network, or touch the installed adapter. `--check` compares all pinned reference/adapter hashes, re-enumerates the inventory, and checks this generated view. `--refresh` changes structural/source evidence; row `annotations` survive only for IDs still present.','',
         '## Scope and evidence rules','', data['method'],'']
    out.extend('- '+x for x in data['limitations'])
    out += ['', '## Actionable facade gaps','',
            'These are individual source findings, not the complete behavior backlog. Unknowns remain unverified. Audited JSON `annotations` override per-axis generated defaults in this view; include exact current source/test/receipt evidence, not historical aggregate passes. `supplemental_rows` admits original-corpus/private-path evidence without changing the pinned structural set.','']
    effective_rows=[effective(r) for r in data['rows']+data.get('supplemental_rows',[])]
    for group in ['ExtensionAPI','ExtensionUIContext','ExtensionContext','ExtensionCommandContext','ReadonlySessionManager']:
        missing=[r['name'] for r in effective_rows if r.get('name','').startswith(group+'.') and r['implementation']['status'] in ['missing-facade-member','unsupported-event','explicit-refusal']]
        out.append('- **'+group+'**: '+', '.join('`'+n+'`' for n in missing)+'.')
    out += ['- `sendMessage` delivery/trigger/display and `sendUserMessage` options require separate semantics; root names existing does not satisfy these fields.',
            '- SDK/CLI/subpath exports are separate rows; root aliases do not prove package lookup, dynamic import, CLI argv/events, authoring types, or child product admission.',
            '', 'Status counts below count rows, **not coverage percentages or passing tests**.','']
    for category,count in sorted(Counter(r['category'] for r in data['rows']).items()):out.append(f'- {category}: {count}')
    out += ['', '## Legend','',
            'I/R/N/O/S = implementation / reachability / native tests / unchanged-original acceptance / synthetic tests. U means unverified. Other codes below are exact statuses, not scores. Full signatures, every overload/reference location, per-axis pointers and manual annotations remain in JSON. Each member/path still has a separate row below.','']
    statuses=sorted({r[k]['status'] for r in effective_rows for k in AXES} - {'unverified'})
    codes={value:'s'+str(i+1) for i,value in enumerate(statuses)}
    codes['unverified']='U'
    out.extend('- `'+code+'`: '+status for status,code in codes.items())
    gaps=sorted({r['gap'] for r in effective_rows})
    gap_codes={g:'g'+str(i+1) for i,g in enumerate(gaps)}
    out += ['', '### Gap key','']
    out.extend('- `'+code+'`: '+gap for gap,code in gap_codes.items())
    files=sorted({x['file'] for r in effective_rows for x in r['reference']})
    file_codes={f:'f'+str(i+1) for i,f in enumerate(files)}
    out += ['', '### Source key','', 'Reference files resolve in pinned Pi Git unless prefixed `extensions/` or `artifacts/` (candidate). `corpus/NAME/path` resolves against the original package root and file SHA-256 in JSON original_corpus; no original package code is executed.','']
    out.extend('- `'+code+'`: `'+file+'`' for file,code in file_codes.items())
    out += ['', '## Complete matrix','',
            'Rows use source keys to avoid repeating long paths. A reference like f10:50 is file f10, line 50. For lexical test navigation and all candidate implementation pointers, find the same ID in JSON. Repeated generic text is represented by the legend, never a coverage claim.','']
    def cell(x):return str(x).replace('|','\\|').replace('\n',' ')
    for category in sorted({r['category'] for r in effective_rows}):
        out += ['### '+category,'','| Member/path | Ref | I | R | N | O | S | Gap |','| --- | --- | --- | --- | --- | --- | --- | --- |']
        for r in effective_rows:
            if r['category']!=category:continue
            identifier=r['id']
            file,sep,name=identifier.partition('#')
            if sep and file in file_codes:identifier=file_codes[file]+'#'+name
            ref=r['reference'][0]
            values=[f'`{identifier}`',f"{file_codes[ref['file']]}:{ref['line']}"]+[codes[r[k]['status']] for k in AXES]+[gap_codes[r['gap']]]
            out.append('| '+' | '.join(cell(x) for x in values)+' |')
        out.append('')
    return '\n'.join(out)


def serialize(data):
    # Authoritative, deterministic JSON: one independently reviewable row/line.
    entries=[]
    for key,value in data.items():
        if key in ['rows','supplemental_rows']:
            encoded='[\n'+',\n'.join('    '+json.dumps(r,ensure_ascii=False,separators=(',',':')) for r in value)+'\n  ]'
        else:
            encoded=json.dumps(value,ensure_ascii=False,sort_keys=True,separators=(',',':'))
        entries.append('  '+json.dumps(key)+': '+encoded)
    return '{\n'+',\n'.join(entries)+'\n}\n'


def self_test():
    source='''interface Hidden { nope: string } interface Base { inherited(): void }
export interface API extends Base { nested(options: { one: string; two?: { three: boolean } }): {result: string}; }
export interface ExtensionAPI { on(event: "alpha", handler: () => void): void; on(event: "beta", handler: () => void): void; }
export type Value = "x"; export { type Value as Other };'''
    parsed=ast({'probe.ts':source})['probe.ts']
    names={r['name'] for r in parsed['rows']}
    assert 'Hidden' not in names and 'Base.inherited' in names
    assert 'API.nested.options.two.three' in names and 'API.nested.return.result' in names
    assert {'ExtensionAPI.on("alpha")','ExtensionAPI.on("beta")'} <= names
    row={'id':'test','native_tests':{'status':'unverified','evidence':[]},
         'annotations':{'native_tests':{'status':'source-only-not-run','evidence':['test.rs:10']}}}
    assert effective(row)['native_tests']['status']=='source-only-not-run'
    assert json.loads(serialize({'rows':[row]}))=={'rows':[row]}
    assert next(e for e in parsed['exports'] if e['name']=='Other')['kind']=='type'
    print('PASS AST reachable types, nested fields/returns, literal event overloads, type exports, annotations and serialization')


def main():
    parser=argparse.ArgumentParser(description=__doc__)
    mode=parser.add_mutually_exclusive_group()
    mode.add_argument('--check',action='store_true')
    mode.add_argument('--refresh',action='store_true')
    mode.add_argument('--render',action='store_true')
    mode.add_argument('--self-test',action='store_true')
    parser.add_argument('--pi-repo',default=DEFAULT_PI,type=Path)
    args=parser.parse_args()
    if args.self_test:
        self_test(); return 0
    old=json.loads(MATRIX.read_text()) if MATRIX.exists() else {}
    if args.render:
        VIEW.write_text(render(old)); print('Generated docs/pi-api-coverage.md'); return
    data=build(args.pi_repo)
    if old.get('supplemental_rows'):
        data['supplemental_rows']=old['supplemental_rows']
        ids={r['id'] for r in data['rows']}
        for row in data['supplemental_rows']:
            if row['id'] in ids:raise ValueError('duplicate supplemental ID: '+row['id'])
            ids.add(row['id'])
            for key in ['category','reference','name','gap']+AXES:
                if key not in row:raise ValueError('incomplete supplemental row: '+row['id'])
    annotations={r['id']:r['annotations'] for r in old.get('rows',[]) if 'annotations' in r}
    for r in data['rows']:
        if r['id'] in annotations:r['annotations']=annotations[r['id']]
    for row in data['rows']+data.get('supplemental_rows',[]):effective(row)
    if args.refresh:
        MATRIX.write_text(serialize(data))
        VIEW.write_text(render(data))
        print(f"Wrote {len(data['rows'])} rows; {len(data['reference']['files'])} pinned reference files. No behavior qualification.")
    else:
        if data!=old:
            old_ids={r['id'] for r in old.get('rows',[])}; ids={r['id'] for r in data['rows']}
            print(f'Inventory/source drift: added={len(ids-old_ids)} removed={len(old_ids-ids)}; review then --refresh',file=sys.stderr)
            return 1
        if not VIEW.exists() or VIEW.read_text()!=render(old):
            print('Generated Markdown drift; run --render',file=sys.stderr); return 1
        print(f"PASS: {len(data['rows'])} structural rows, reference/adapter hashes and generated view match. No behavior tests run.")
    return 0


if __name__=='__main__':
    sys.exit(main())
