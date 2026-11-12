"""Offline process-level release lifecycle with persistent fake gh/git boundaries."""
import copy
import hashlib
import json
import os
from pathlib import Path
import re
import subprocess
import sys
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(ROOT / "scripts"))
import release

SHA = "a" * 40
VERSION = "1.2.3"
FAKE = r'''#!/usr/bin/env python3
import hashlib, json, os, pathlib, sys
p = pathlib.Path(os.environ['FAKE_STATE'])
s = json.loads(p.read_text())
a = sys.argv[1:]
tool = pathlib.Path(sys.argv[0]).name
with open(os.environ['FAKE_CALLS'], 'a') as f: f.write(json.dumps([tool, *a])+'\n')
def out(x): print(json.dumps(x))
def finish(): p.write_text(json.dumps(s))
if tool == 'git':
    if a == ['status', '--porcelain']: print(s.get('dirty',''))
    elif a == ['rev-parse', 'HEAD']: print(s['head'])
    elif a == ['rev-parse', 'refs/remotes/origin/main']: print(s['main'])
    else: sys.exit('unexpected git: '+repr(a))
elif tool == 'cosign':
    if s.get('bad_signature'): sys.exit(1)
elif a[:2] == ['release', 'upload']:
    r = s['releases'][0]
    file = pathlib.Path(a[-1]); data = file.read_bytes()
    if any(x['name'] == file.name for x in r['assets']): sys.exit('duplicate upload')
    identity = s.get('next_asset',1); s['next_asset'] = identity+1
    r['assets'].append(dict(id=identity,name=file.name,digest='sha256:'+hashlib.sha256(data).hexdigest()))
    s.setdefault('downloads',{})[str(identity)] = data.hex()
    finish()
    if s.pop('fail_upload_after',False): finish(); sys.exit(1)
elif a[:2] == ['workflow', 'run']:
    workflow = a[2]
    inputs = dict(x.split('=',1) for i,x in enumerate(a) if i and a[i-1]=='-f')
    gen = inputs['candidate']
    version = inputs.get('version', inputs.get('release_tag','v1.2.3')[1:])
    operation = 'build' if workflow == 'release-candidate.yml' else 'extensions' if workflow == 'release-serve.yml' else 'ship'
    s['runs'].append(dict(id=len(s['runs'])+50, display_title=f'octet v{version} {gen} {operation}',
                         workflow=workflow, event='workflow_dispatch', head_branch='main' if operation=='build' else a[a.index('--ref')+1],
                         head_sha=s['main'], path='.github/workflows/'+workflow,
                         status='completed' if operation=='extensions' else 'in_progress', conclusion='success' if operation=='extensions' else None))
    finish()
    if s.pop('fail_dispatch_after',False): finish(); sys.exit(1)
elif a[:2] == ['run','cancel']:
    next(r for r in s['runs'] if str(r['id']) == a[2]).update(status='completed',conclusion='cancelled')
elif a[:2] == ['run','rerun']:
    assert '--failed' in a
    next(r for r in s['runs'] if str(r['id']) == a[2]).update(status='in_progress',conclusion=None)
    s['reruns'] = s.get('reruns',0)+1
elif a[:2] == ['run','watch']:
    run=next(r for r in s['runs'] if str(r['id'])==a[2])
    if run['conclusion'] != 'success': sys.exit(1)
elif a[0]=='api':
    endpoint=a[1].split('skaft-software/octet/',1)[1]
    method=a[a.index('--method')+1]
    data=json.loads(sys.stdin.read()) if '--input' in a else {}
    if endpoint.startswith('releases/assets/'):
        assert method=='GET'
        sys.stdout.buffer.write(bytes.fromhex(s['downloads'][endpoint.rsplit('/',1)[1]]))
    elif endpoint.split('?')[0]=='releases':
        if method=='GET': out([s['releases']])
        elif method=='POST':
            r=dict(data,id=1,assets=[]); s['releases'].append(r); out(r)
            finish()
            if s.pop('fail_create_after',False): finish(); sys.exit(1)
        else: sys.exit('unexpected releases method')
    elif endpoint=='releases/1':
        if method=='PATCH': s['releases'][0].update(data); out(s['releases'][0])
        elif method=='DELETE': s['releases']=[]
        else: out(s['releases'][0])
    elif endpoint.startswith('git/matching-refs/tags/'):
        tag=endpoint.split('tags/',1)[1]
        out([dict(ref='refs/tags/'+k) for k in s['tags'] if k.startswith(tag)])
    elif endpoint=='git/refs':
        tag=data['ref'][10:]
        assert tag not in s['tags'], 'never recreate tags'
        s['tags'][tag]=data['sha']; out(data)
        finish()
        if s.pop('fail_tag_after',False): finish(); sys.exit(1)
    elif endpoint.startswith('commits/'): out(dict(sha=s['tags'][endpoint.split('/',1)[1]]))
    elif endpoint.startswith('compare/'): out(dict(status=s.get('ancestry','identical'),total_commits=0,commits=[]))
    elif '/ci.yml/runs?' in endpoint:
        rows=s.get('ci_runs',[dict(id=42,head_sha=s['main'],event='push',head_branch='main',status='completed',conclusion=s.get('ci','success'))])
        out([dict(workflow_runs=rows)])
    elif '/jobs?' in endpoint: out([dict(jobs=s['jobs'])])
    elif endpoint.startswith('actions/workflows/'):
        workflow=endpoint.split('/')[2]
        out([dict(workflow_runs=[r for r in s['runs'] if r['workflow']==workflow])])
    else: sys.exit('unexpected endpoint: '+endpoint)
else: sys.exit('unexpected command: '+repr(a))
finish()
'''


class ReleaseCLITests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory(prefix="octet-release-test-")
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        for tool in ("gh", "git", "cosign"):
            path = self.root / tool
            path.write_text(FAKE)
            path.chmod(0o755)
        self.path = self.root / "state.json"
        self.calls = self.root / "calls"
        self.data = dict(head=SHA, main=SHA, releases=[], runs=[], tags={}, jobs=[dict(name=n,conclusion="success") for n in release.LANES])
        self.save()
        self.env = dict(os.environ, PATH=str(self.root)+os.pathsep+os.defpath, HOME=str(self.root), TMPDIR=str(self.root),
                        FAKE_STATE=str(self.path), FAKE_CALLS=str(self.calls), GITHUB_STEP_SUMMARY=str(self.root/'summary'))
        (self.root / "Cargo.toml").write_text('[workspace.package]\nversion = "1.2.3"\n')
        (self.root / "CHANGELOG.md").write_text('# Changelog\n\n## [Unreleased]\n\n## [1.2.3] - 2026-11-12\n\n### Fixed\n\n- Editing works.\n')
        release.notes.sync(self.root, self.root / "CHANGELOG.md")

    def save(self):
        self.path.write_text(json.dumps(self.data))

    def cli(self, *args, ok=True):
        result = subprocess.run([sys.executable, str(ROOT / "scripts/release.py"), args[0], VERSION, *args[1:]],
                                cwd=self.root, env=self.env, text=True, capture_output=True, timeout=20)
        self.data = json.loads(self.path.read_text())
        self.assertEqual(result.returncode == 0, ok, result.stdout + result.stderr)
        return result

    def receipt(self):
        r = self.data['releases'][0]
        keys=release.MARKER.findall(r['body'])
        name='candidate-receipt-'+keys[0]+'.json' if keys else 'ship-receipt.json'
        asset=next(a for a in r['assets'] if a['name']==name)
        return json.loads(bytes.fromhex(self.data['downloads'][str(asset['id'])]))

    def started(self):
        self.cli('start')
        return self.receipt()

    def attach(self, content='first', finish=True):
        state = self.receipt()
        directory = self.root / 'candidate-assets'
        directory.mkdir(exist_ok=True)
        for path in directory.iterdir(): path.unlink()
        for name in release.expected(VERSION): (directory / name).write_text(content+name)
        run=self.data['runs'][0]
        self.env.update(GITHUB_RUN_ID=str(run['id']), GITHUB_REF='refs/heads/main', GITHUB_SHA=SHA)
        self.cli('_attach','--candidate',state['generation'],'--assets',str(directory))
        if finish:
            self.data['runs'][0].update(status='completed',conclusion='success')
            self.save()
        return state

    def ready(self):
        self.started()
        return self.attach()

    def calls_json(self):
        return [json.loads(x) for x in self.calls.read_text().splitlines()]

    def test_prepare_is_local_read_only_and_repeatable(self):
        self.cli('prepare'); self.cli('prepare')
        self.assertTrue(all(c[0]=='git' for c in self.calls_json()))
        self.assertEqual([],self.data['releases'])

    def test_prepare_refuses_dirty_wrong_head_version_undated_or_stale_notes(self):
        for field in ('dirty','head'):
            original=self.data[field] if field in self.data else ''
            self.data[field]='b'*40; self.save(); self.cli('prepare',ok=False)
            self.data[field]=original; self.save()
        text=(self.root/'CHANGELOG.md').read_text()
        (self.root/'CHANGELOG.md').write_text(text.replace('2026-11-12','Release candidate'))
        self.cli('prepare',ok=False)
        (self.root/'CHANGELOG.md').write_text(text)
        (self.root/'docs/releases/v1.2.3.md').write_text('stale')
        self.cli('prepare',ok=False)
        (self.root/'Cargo.toml').write_text('[workspace.package]\nversion = "1.2.4"\n')
        self.cli('prepare',ok=False)

    def test_start_is_one_exact_sha_unsigned_candidate_and_no_tag(self):
        state=self.started(); self.cli('start'); self.cli('resume')
        self.assertEqual(1,len(self.data['runs']))
        dispatch=next(c for c in self.calls_json() if c[1:3]==['workflow','run'])
        self.assertIn('source_commit='+SHA,dispatch); self.assertIn('main',dispatch)
        self.assertEqual({},self.data['tags']); self.assertTrue(self.data['releases'][0]['draft'])
        self.assertEqual(42,state['ci_run']); self.assertFalse(state['complete'])
        self.assertTrue(any(a['name'].startswith('candidate-receipt-') for a in self.data['releases'][0]['assets']))

    def test_dispatch_interruption_reconciles_without_duplicate(self):
        self.data['fail_dispatch_after']=True; self.save()
        self.cli('start',ok=False); self.cli('start'); self.cli('resume')
        self.assertEqual(1,len(self.data['runs']))

    def test_initial_receipt_upload_interruption_is_recoverable(self):
        self.data['fail_upload_after']=True; self.save()
        self.cli('start',ok=False); self.cli('start')
        self.assertEqual(1,len(self.data['releases'])); self.assertEqual(1,len(self.data['runs']))

    def test_draft_creation_interruption_recovers_seed_without_second_draft(self):
        self.data['fail_create_after']=True; self.save()
        self.cli('start',ok=False); self.cli('start')
        self.assertEqual(1,len(self.data['releases'])); self.assertEqual(1,len(self.data['runs']))

    def test_ambiguous_dispatch_does_not_replay_when_actions_is_not_visible(self):
        self.started(); self.data['runs']=[]; self.save()
        self.cli('start',ok=False); self.cli('resume',ok=False)
        self.assertEqual(1,sum(c[1:3]==['workflow','run'] for c in self.calls_json()))

    def test_status_reconstructs_complete_from_receipt_and_runs(self):
        self.ready(); status=json.loads(self.cli('status').stdout)
        self.assertEqual('complete',status['state']); self.assertTrue(status['receipt']['complete'])
        self.assertEqual([50],status['receipt']['run_ids'])

    def test_pause_cancels_and_blocks_ship_until_resume(self):
        self.started(); self.cli('pause')
        self.assertTrue(self.receipt()['paused']); self.assertEqual('cancelled',self.data['runs'][0]['conclusion'])
        self.assertEqual('paused',json.loads(self.cli('status').stdout)['state'])
        self.cli('ship',ok=False); self.cli('resume')
        self.assertFalse(self.receipt()['paused']); self.assertEqual(1,self.data['reruns'])
        self.assertEqual(1,len(self.data['runs']))

    def test_resume_keeps_successful_builds_and_reruns_only_failed_jobs(self):
        self.ready(); self.cli('pause'); self.cli('resume'); self.cli('resume')
        self.assertNotIn('reruns',self.data)
        self.assertTrue(self.receipt()['complete'])
        self.data['runs'][0].update(status='completed',conclusion='failure'); self.save()
        # Even a previously reconciled complete receipt must not let a failed run ship.
        self.cli('ship',ok=False)

    def test_atomic_respin_changes_generation_and_digest_set(self):
        old=self.ready(); self.cli('resume'); old=self.receipt()
        self.cli('respin'); new=self.receipt()
        self.assertNotEqual(old['generation'],new['generation']); self.assertFalse(new['complete'])
        # Old assets remain; only a complete new build selects its replacement digests.
        old_assets=copy.deepcopy(old['digests'])
        self.data['runs'].reverse(); self.save()
        self.attach('replacement')
        status=json.loads(self.cli('status').stdout)['receipt']
        self.assertNotEqual(old_assets,status['digests'])
        self.assertEqual(release.expected(VERSION),set(status['digests']))
        self.assertTrue(all(e['asset'] in release.assets(self.data['releases'][0]) for e in old_assets.values()))

    def test_new_sha_respin_requalifies_ci(self):
        self.ready(); self.data.update(head='b'*40,main='b'*40,ci='failure'); self.save()
        self.cli('respin','--sha','b'*40,ok=False)
        self.assertEqual(SHA,self.receipt()['sha'])
        self.data['ci']='success'; self.save(); self.cli('respin','--sha','b'*40)
        self.assertEqual('b'*40,self.receipt()['sha'])

    def test_worker_cannot_clear_pause_or_attach_to_stale_generation(self):
        state=self.started(); self.cli('pause')
        self.env.update(GITHUB_RUN_ID='50',GITHUB_REF='refs/heads/main')
        self.cli('_attach','--candidate',state['generation'],'--assets',str(self.root),ok=False)
        self.assertTrue(self.receipt()['paused'])
        self.cli('respin')
        self.cli('_attach','--candidate',state['generation'],'--assets',str(self.root),ok=False)

    def test_ci_not_green_or_not_main_push_is_refused_without_writes(self):
        for event,branch,status,outcome in [('workflow_dispatch','main','completed','success'),('pull_request','main','completed','success'),
                                           ('push','feature','completed','success'),('push','main','in_progress',None),('push','main','completed','failure')]:
            self.data['ci_runs']=[dict(id=42,head_sha=SHA,event=event,head_branch=branch,status=status,conclusion=outcome)]
            self.save(); self.cli('start',ok=False); self.assertEqual([],self.data['releases'])

    def test_each_required_lane_is_successful_exactly_once(self):
        for lane in release.LANES:
            for outcome in ('missing','skipped','failure','duplicate'):
                self.data['jobs']=[dict(name=n,conclusion='success') for n in release.LANES if n!=lane]
                if outcome!='missing': self.data['jobs'].append(dict(name=lane,conclusion='success' if outcome=='duplicate' else outcome))
                if outcome=='duplicate': self.data['jobs'].append(dict(name=lane,conclusion='success'))
                self.save(); self.cli('start',ok=False); self.assertEqual([],self.data['releases'])

    def test_ship_checks_digests_and_tag_collisions_before_any_tag_creation(self):
        self.ready()
        self.data['tags']['octet-serve-v1.2.3']='b'*40; self.save()
        self.cli('ship',ok=False); self.assertEqual(1,len(self.data['tags']))
        self.data['tags']={}; self.data['releases'][0]['assets'][-2]['digest']='sha256:'+'0'*64; self.save()
        self.cli('ship',ok=False); self.assertEqual({},self.data['tags'])

    def test_ship_requires_reasoned_skip_and_freezes_decision(self):
        self.ready(); self.cli('ship','--skip-npm',ok=False)
        self.cli('ship','--skip-npm','--reason','registry incident')
        state=self.receipt(); self.assertEqual('registry incident',state['skip_npm'])
        self.cli('ship','--skip-npm','--reason','changed',ok=False)
        self.assertEqual({SHA},set(self.data['tags'].values()))
        self.assertEqual(3,len(self.data['tags']))
        refs=[c[c.index('--ref')+1] for c in self.calls_json() if c[1:3]==['workflow','run']]
        self.assertEqual(['main','octet-serve-v1.2.3','octet-binaries-v1.2.3'],refs)

    def test_ship_resumes_after_partial_tag_creation_without_recreating_refs(self):
        self.ready(); self.data['fail_tag_after']=True; self.save()
        self.cli('ship',ok=False); self.cli('ship'); self.cli('ship')
        refs=[c for c in self.calls_json() if c[1]=='api' and c[2].endswith('/git/refs')]
        self.assertEqual(3,len(refs)); self.assertEqual(3,len(self.data['runs']))

    def test_ship_dispatch_interruption_reconciles_without_duplicate_signing(self):
        self.ready(); self.data['fail_dispatch_after']=True; self.save()
        self.cli('ship',ok=False); self.cli('ship'); self.cli('ship')
        self.assertEqual(3,len(self.data['runs']))
        self.data['runs']=[r for r in self.data['runs'] if r['workflow']!='release-serve.yml']; self.save()
        self.cli('ship',ok=False)
        self.assertEqual(3,sum(c[1:3]==['workflow','run'] for c in self.calls_json()))

    def test_downloaded_bytes_not_just_metadata_are_checked_before_tags(self):
        self.ready()
        asset=next(a for a in self.data['releases'][0]['assets'] if a['name'].endswith('.tgz'))
        self.data['downloads'][str(asset['id'])]=b'tampered'.hex(); self.save()
        self.cli('ship',ok=False); self.assertEqual({},self.data['tags'])

    def test_signed_publication_retry_preserves_assets_and_receipts(self):
        self.ready(); self.cli('ship'); state=self.receipt(); gen=state['generation']
        candidate=self.root/'candidate-assets'
        ext=self.root/'extensions'; ext.mkdir()
        for name in release.groups(VERSION)['extensions']:
            (ext/name).write_bytes((candidate/name).read_bytes())
        (ext/'SHA256SUMS').write_text('fixture checksums')
        for path in list(ext.iterdir()): (ext/(path.name+'.sigstore.json')).write_text('fixture signature')
        self.env.update(GITHUB_REF='refs/tags/octet-serve-v1.2.3',GITHUB_SHA=SHA)
        self.data['fail_upload_after']=True; self.save()
        self.cli('_signed','--candidate',gen,'--role','extensions','--assets',str(ext),ok=False)
        self.cli('_signed','--candidate',gen,'--role','extensions','--assets',str(ext))
        for path in ext.glob('*.sigstore.json'): path.write_text('fresh signature')
        self.cli('_signed','--candidate',gen,'--role','extensions','--assets',str(ext))
        self.data['bad_signature']=True; self.save()
        self.cli('_signed','--candidate',gen,'--role','extensions','--assets',str(ext),ok=False)
        self.data['bad_signature']=False; self.save()
        native=self.root/'native'; native.mkdir()
        for name in release.groups(VERSION)['native']:
            (native/name).write_bytes((candidate/name).read_bytes())
        for name in ('install-octet.sh','install-octet.ps1','OCTET_SHA256SUMS','OCTET_RELEASE_METADATA.json'):
            (native/name).write_text('fixture '+name)
        for path in list(native.iterdir()): (native/(path.name+'.sigstore.json')).write_text('fixture signature')
        (native/'release.json').write_bytes((candidate/'release.json').read_bytes())
        self.env['GITHUB_REF']='refs/tags/octet-binaries-v1.2.3'
        self.cli('_publish','--candidate',gen,'--role','ship','--assets',str(native))
        published=copy.deepcopy(self.data['releases'][0])
        self.assertFalse(published['draft']); self.assertEqual('false',published['make_latest'])
        self.assertEqual(state['notes'],published['body'])
        self.cli('_publish','--candidate',gen,'--role','ship','--assets',str(native))
        self.cli('ship'); self.assertEqual(published,self.data['releases'][0])
        (native/'install-octet.sh').write_text('changed')
        self.cli('_publish','--candidate',gen,'--role','ship','--assets',str(native),ok=False)
        self.assertEqual(published,self.data['releases'][0])
        self.assertEqual('published',json.loads(self.cli('status').stdout)['state'])

    def test_incomplete_non_main_or_paused_candidate_never_ships(self):
        self.started(); self.cli('ship',ok=False)
        self.attach(); self.data['ancestry']='diverged'; self.save(); self.cli('ship',ok=False)
        self.data['ancestry']='identical'; self.save(); self.cli('pause'); self.cli('ship',ok=False)
        self.assertEqual({},self.data['tags'])

    def test_abort_is_idempotent_and_status_reports_absent_or_aborted(self):
        self.started(); self.cli('abort'); self.cli('abort')
        self.assertEqual([],self.data['releases']); self.assertEqual({},self.data['tags'])
        self.assertEqual('absent-or-aborted',json.loads(self.cli('status').stdout)['state'])
        self.cli('resume',ok=False)

    def test_abort_refuses_published_or_tagged_candidates(self):
        self.started(); self.data['tags']['v1.2.3']=SHA; self.save(); self.cli('abort',ok=False)
        self.data['tags']={}; self.data['releases'][0]['draft']=False; self.save(); self.cli('abort',ok=False)

    def test_ship_signer_role_ref_and_sha_are_enforced(self):
        self.ready(); self.cli('ship')
        gen=self.receipt()['generation']
        for role,ref in [('ship','octet-binaries-v1.2.3'),('extensions','octet-serve-v1.2.3')]:
            self.env.update(GITHUB_REF='refs/heads/main',GITHUB_SHA=SHA)
            self.cli('_resolve','--candidate',gen,'--role',role,ok=False)
            self.env['GITHUB_REF']='refs/tags/'+ref
            self.cli('_resolve','--candidate',gen,'--role',role)
            self.env['GITHUB_SHA']='b'*40
            self.cli('_resolve','--candidate',gen,'--role',role,ok=False)


if __name__ == '__main__':
    unittest.main()
