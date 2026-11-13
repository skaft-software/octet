"""No-network contracts for the candidate/sign/publish dependency graph and policy."""
import hashlib
from importlib import import_module
import os
from pathlib import Path
import re
import shutil
import subprocess
import sys
import tempfile
import textwrap
import unittest
from unittest import mock

ROOT = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(ROOT / 'scripts'))
import release
promote = import_module('release-promote')


def jobs(name):
    text = (ROOT / '.github/workflows' / name).read_text()
    return text, dict(re.findall(r'(?ms)^  ([\w-]+):\n(.*?)(?=^  [\w-]+:\n|\Z)', text.split('\njobs:\n', 1)[1]))


def permissions(text, indent):
    block = re.search(rf'(?m)^{" " * indent}permissions:\n((?:{" " * (indent + 2)}[\w-]+: \w+\n)+)', text)
    return dict(re.findall(r'([\w-]+): (\w+)', block[1])) if block else {}


def step(text, name):
    block = text.split('      - name: '+name+'\n',1)[1]
    return textwrap.dedent(block.split('        run: |\n',1)[1].split('\n      - ',1)[0]).rstrip()


class WorkflowContracts(unittest.TestCase):
    def test_no_push_trigger_and_no_runner_workflow_polling(self):
        for name in ('release-candidate.yml','release-octet.yml','release-serve.yml'):
            text, graph = jobs(name)
            header = text.split('\njobs:\n',1)[0]
            triggers = header.split('\non:\n',1)[1].split('\nconcurrency:',1)[0]
            self.assertEqual(['workflow_dispatch'], re.findall(r'(?m)^  ([\w-]+):', triggers))
            self.assertNotRegex(graph['resolve'],r'\bsleep\b|for attempt|while |run watch')
            self.assertNotIn('timeout-minutes: 85',text)
            self.assertNotIn('timeout-minutes: 360',text)
        self.assertNotIn('release-octet.yml/runs',jobs('release-serve.yml')[1]['resolve'])

    def test_exact_least_privilege_job_permissions(self):
        read = {'actions': 'read', 'contents': 'read'}
        write = {'actions': 'read', 'contents': 'write'}
        sign = {**write, 'id-token': 'write'}
        overrides = {
            'release-candidate.yml': {'resolve': write, 'attach': write},
            'release-serve.yml': {'resolve': write, 'publish': sign},
            'release-octet.yml': {
                'resolve': write, 'publish': sign, 'promote-latest': write,
                'package-npm': read, 'publish-npm': sign,
                'verify-npm-published': {'contents': 'read'},
                'publish-installer-repair': {'contents': 'write', 'id-token': 'write'},
            },
        }
        for workflow, expected in overrides.items():
            text, graph = jobs(workflow)
            with self.subTest(workflow=workflow):
                self.assertEqual(read, permissions(text, 0))
                self.assertEqual(expected, {name: value for name, job in graph.items()
                                            if (value := permissions(job, 4))})

    def test_draft_reading_commands_have_write_without_widening_public_readers(self):
        seen = set()
        for workflow in ('release-candidate.yml', 'release-serve.yml', 'release-octet.yml'):
            text, graph = jobs(workflow)
            for name, job in graph.items():
                if not re.search(r'\bscripts/release\.py\s+_(?:resolve|download|attach|signed|publish)\b', job):
                    continue
                seen.add((workflow, name))
                effective = permissions(job, 4) or permissions(text, 0)
                with self.subTest(workflow=workflow, job=name):
                    if workflow == 'release-octet.yml' and name == 'package-npm':
                        self.assertEqual('read', effective['contents'])
                        self.assertIn('needs: [resolve, verify-published]', job)
                        self.assertIn("needs['verify-published'].result == 'success'", job)
                    else:
                        self.assertEqual('write', effective['contents'])
                    if workflow == 'release-octet.yml' and name == 'promote-latest':
                        # This post-publication reader still needs write to move Latest.
                        self.assertIn("needs.publish.result == 'success'", job)
                        self.assertIn('gh release edit "$RELEASE_TAG" --latest', job)
        self.assertEqual({
            ('release-candidate.yml', 'resolve'), ('release-candidate.yml', 'attach'),
            ('release-serve.yml', 'resolve'), ('release-serve.yml', 'publish'),
            ('release-octet.yml', 'resolve'), ('release-octet.yml', 'publish'),
            ('release-octet.yml', 'package-npm'), ('release-octet.yml', 'promote-latest'),
        }, seen)
        text, graph = jobs('release-octet.yml')
        for name in ('verify-published', 'verify-windows-published', 'verify-installer-repair'):
            self.assertEqual('read', (permissions(graph[name], 4) or permissions(text, 0))['contents'])

    def test_candidate_is_one_unsigned_source_pinned_native_and_extension_build(self):
        text, graph = jobs('release-candidate.yml')
        self.assertIn("github.ref == 'refs/heads/main'",graph['resolve'])
        self.assertIn('ref: ${{ inputs.source_commit }}',graph['resolve'])
        self.assertNotIn('id-token: write',text)
        self.assertNotIn('stable-release-publish',text)
        self.assertNotIn('cosign sign-blob',text)
        self.assertNotIn('npm publish',text)
        self.assertNotIn('Swatinem/rust-cache',text)
        self.assertIn('needs: [resolve, build, build-windows, build-bundles]',graph['attach'])
        self.assertIn('scripts/package-octet-npm.sh',graph['attach'])
        for name in ('build','build-windows','build-bundles'):
            self.assertIn('needs.resolve.outputs.source_commit',graph[name])
        # Candidate carries the existing build qualification, not a reduced copy.
        old = jobs('release-octet.yml')[1]
        for job in ('build','build-windows'):
            original=old[job].replace("    if: needs.resolve.outputs.mode == 'binaries'\n",'')
            self.assertEqual(original,graph[job])
        ext=jobs('release-serve.yml')[1]['build-bundles'].replace("    if: needs.resolve.outputs.mode != 'ship'\n",'')
        self.assertEqual(ext,graph['build-bundles'])

    def test_ship_signing_jobs_use_receipt_route_tooling_refs_and_protected_environment(self):
        for name,role in [('release-octet.yml','ship'),('release-serve.yml','extensions')]:
            text,graph=jobs(name)
            self.assertIn('--role '+role,graph['resolve'])
            self.assertIn('environment: stable-release-publish',graph['publish'])
            self.assertIn('id-token: write',graph['publish'])
            self.assertIn('needs.resolve.outputs.mode == \'ship\'',graph['publish'])
            self.assertIn('_download "$VERSION"',graph['publish'])
            self.assertIn('cosign sign-blob',graph['publish'])
            self.assertIn('COSIGN_IDENTITY: https://github.com/${{ github.workflow_ref }}',graph['publish'])
            self.assertIn('--certificate-github-workflow-sha "$WORKFLOW_COMMIT"',graph['publish'])
        state={'version':'0.10.4','sha':'a'*40}
        self.assertIn('@refs/tags/octet-binaries-v0.10.4',release.verify_args(state,'ship')[1])
        self.assertIn('@refs/tags/octet-serve-v0.10.4',release.verify_args(state,'extensions')[1])

    def test_latest_depends_on_native_windows_npm_provenance_and_all_install_smokes(self):
        text,graph=jobs('release-octet.yml')
        latest=graph['promote-latest']
        for job in ('publish','verify-published','verify-windows-published','verify-npm-published','smoke-npm-published','smoke-npm-windows'):
            self.assertIn(job,latest)
        self.assertIn("needs.resolve.outputs.skip_npm == 'true'",latest)
        self.assertIn('release-promote.py',latest)
        self.assertLess(latest.index('release-promote.py'),latest.index('gh release edit "$RELEASE_TAG" --latest'))
        self.assertIn('--provenance',graph['publish-npm'])
        self.assertIn('verify-octet-npm-provenance.py',graph['verify-npm-published'])
        self.assertIn('_download "$VERSION"',graph['package-npm'])
        self.assertIn('--group npm',graph['package-npm'])
        self.assertIn('Install exact release and smoke native Windows binaries',graph['verify-windows-published'])
        self.assertIn('Verify official extension installation and update',graph['verify-published'])

    def test_manual_release_uses_shared_github_renderer_and_title(self):
        publish = jobs('release-octet.yml')[1]['publish']
        self.assertIn('python3 scripts/release-notes.py render "$VERSION" --github', publish)
        self.assertIn('--draft --title "octet $VERSION"', publish)
        self.assertIn('--draft=false --latest=false --title "octet $VERSION"', publish)
        self.assertNotIn('cat "docs/releases/$RELEASE_TAG.md"', publish)
        self.assertIn('octet-binary-release-run:', publish)

    def test_ship_jobs_downstream_of_skipped_builds_still_run(self):
        # Ship mode skips build jobs; GitHub then skips every dependent job that
        # lacks always(), which would silently block Latest promotion.
        text, graph = jobs('release-octet.yml')
        for name, job in graph.items():
            needs = re.search(r"^    needs: \[?([^\]\n]+)\]?$", job, re.M)
            condition = re.search(r"^    if: (.+)$", job, re.M)
            direct = [n.strip() for n in needs.group(1).split(',')] if needs else []
            if not condition or "'ship'" not in condition.group(1) or direct == ['resolve']:
                continue
            with self.subTest(job=name):
                self.assertTrue(condition.group(1).startswith("always() && needs.resolve.result == 'success'"), name)
        for name, need in (('verify-windows-published', 'publish'), ('smoke-npm-windows', 'verify-npm-published')):
            self.assertIn("needs['%s'].result == 'success'" % need if '-' in need else "needs.%s.result == 'success'" % need, graph[name])

    def test_windows_release_checkouts_keep_repository_line_endings(self):
        # npm packages every platform's LICENSE and docs byte-for-byte; a CRLF
        # Windows checkout would make the archives differ.
        for workflow in ('release-candidate.yml', 'release-octet.yml', 'release-serve.yml'):
            text, graph = jobs(workflow)
            for name, job in graph.items():
                if 'runs-on: windows' not in job or 'actions/checkout@' not in job:
                    continue
                with self.subTest(workflow=workflow, job=name):
                    self.assertIn('git config --global core.autocrlf false', job)
                    self.assertLess(job.index('core.autocrlf false'), job.index('actions/checkout@'))

    def test_trusted_publishing_job_uses_a_supported_node(self):
        text, graph = jobs('release-octet.yml')
        version = re.search(r'node-version: "(\d+)\.(\d+)\.(\d+)"', graph['publish-npm'])
        self.assertGreaterEqual(tuple(map(int, version.groups())), (22, 14, 0))

    def test_actual_new_installer_regex_accepts_ship_identity_without_loosening(self):
        text=(ROOT/'scripts/install.sh').read_text()
        block='signing_repository="$repository"'+text.split('    signing_repository="$repository"',1)[1].split('    python3 - \\\n',1)[0]
        script='repository="skaft-software/octet"\nversion="0.10.4"\nrelease_source_commit="'+('a'*40)+'"\n'+block+'\nprintf "%s\\n" "$identity"\n'
        pattern=subprocess.check_output(['sh','-eu','-c',script],text=True,env={'PATH':os.defpath}).strip()
        signer=release.verify_args({'version':'0.10.4','sha':'a'*40},'ship')[1]
        self.assertRegex(signer,pattern)
        self.assertNotRegex(signer.replace('refs/tags/octet-binaries-v0.10.4','refs/heads/main'),pattern)
        self.assertNotRegex(signer.replace('0.10.4','0.10.5'),pattern)

    def test_actual_extension_workflow_verifier_accepts_ship_identity_and_sha(self):
        text,_=jobs('release-serve.yml')
        script=step(text,'Verify checksums and signatures').split('\n  verify-published:',1)[0]
        with tempfile.TemporaryDirectory() as temporary:
            root=Path(temporary); (root/'release-assets').mkdir()
            archive=root/'release-assets/bundle.tar.gz'; archive.write_bytes(b'fixture')
            (root/'release-assets/SHA256SUMS').write_text(hashlib.sha256(b'fixture').hexdigest()+'  ./bundle.tar.gz\n')
            if shutil.which('sha256sum', path=os.defpath) is None:
                # The workflow runs on Linux; only the local test needs a shim.
                checksum = root/'sha256sum'
                checksum.write_text('''#!/usr/bin/env python3
import hashlib, pathlib, sys
assert sys.argv[1:] == ['--check', 'SHA256SUMS']
for line in pathlib.Path('SHA256SUMS').read_text().splitlines():
    expected, name = line.split('  ', 1)
    if hashlib.sha256(pathlib.Path(name).read_bytes()).hexdigest() != expected:
        sys.exit('checksum mismatch: '+name)
''')
                checksum.chmod(0o755)
            tool=root/'cosign'
            tool.write_text('''#!/usr/bin/env python3
import json, os, sys
with open(os.environ['CALLS'],'a') as stream: stream.write(json.dumps(sys.argv[1:])+'\\n')
''')
            tool.chmod(0o755)
            import json
            state={'version':'0.10.4','sha':'a'*40}
            env=dict(os.environ,PATH=str(root)+os.pathsep+os.defpath,
                     COSIGN_IDENTITY=release.verify_args(state,'extensions')[1],
                     WORKFLOW_COMMIT=state['sha'], WORKFLOW_REPOSITORY=release.REPO,
                     COSIGN_OIDC_ISSUER='https://token.actions.githubusercontent.com',CALLS=str(root/'calls'))
            result=subprocess.run(['bash','-eu','-c',script],cwd=root,env=env,text=True,capture_output=True)
            self.assertEqual(0,result.returncode,result.stdout+result.stderr)
            for line in (root/'calls').read_text().splitlines():
                args=json.loads(line)
                for flag,value in zip(release.verify_args(state,'extensions')[::2],release.verify_args(state,'extensions')[1::2]):
                    self.assertEqual(value,args[args.index(flag)+1])
            calls = (root/'calls').read_bytes()
            archive.write_bytes(b'tampered')
            result=subprocess.run(['bash','-eu','-c',script],cwd=root,env=env,text=True,capture_output=True)
            self.assertNotEqual(0,result.returncode)
            self.assertEqual(calls, (root/'calls').read_bytes(), 'bad checksums must stop before signature checks')
        # The extension client has no certificate regex. Don't invent one or
        # weaken its separate HTTPS/checksum/trust boundary to satisfy a test.
        client=(ROOT/'crates/octet-coding-agent/src/extension_bundle.rs').read_text()
        self.assertIn('checksum_for_asset(&checksums, &asset)?',client)
        self.assertIn('if actual != expected',client)

    def test_extension_workflow_verifier_without_system_sha256sum(self):
        # Exercise the macOS boundary even on a host with GNU coreutils.
        with tempfile.TemporaryDirectory() as temporary:
            for name in ('bash', 'python3'):
                (Path(temporary)/name).symlink_to(shutil.which(name))
            with mock.patch.object(os, 'defpath', temporary):
                self.test_actual_extension_workflow_verifier_accepts_ship_identity_and_sha()

    def test_opt_in_provider_gate_still_needs_exact_sha_success_and_approval(self):
        run=dict(id=91,head_sha='a'*40,event='workflow_dispatch',status='completed',conclusion='success')
        def api(path, **kwargs):
            return [dict(workflow_runs=[run])] if '/runs?' in path else [dict(state='approved',environments=[dict(name='stable-release-provider-acceptance')])]
        with mock.patch.object(release,'api',side_effect=api):
            self.assertEqual(91,release.provider_acceptance('a'*40))
            run['head_sha']='b'*40
            with self.assertRaisesRegex(ValueError,'candidate SHA'): release.provider_acceptance('a'*40)
        run['head_sha']='a'*40
        with mock.patch.object(release,'api',side_effect=[[dict(workflow_runs=[run])],[]]):
            with self.assertRaisesRegex(ValueError,'approval'): release.provider_acceptance('a'*40)

    def test_promotion_skip_needs_recorded_reason_otherwise_checks_all_dist_tags(self):
        state=dict(shipping=True,paused=False,skip_npm='')
        record=dict(draft=False,prerelease=False)
        with mock.patch.object(promote,'get_release',return_value=record), mock.patch.object(promote,'load',return_value=state), mock.patch.object(promote,'command',return_value='{"latest":"1.2.3"}') as cmd:
            promote.check('1.2.3'); self.assertEqual(5,cmd.call_count)
            cmd.return_value='{"latest":"1.2.2"}'
            with self.assertRaisesRegex(ValueError,'npm latest differs'): promote.check('1.2.3')
            state['skip_npm']='  '
            with self.assertRaisesRegex(ValueError,'reason'): promote.check('1.2.3')
            state['skip_npm']='registry incident'
            with tempfile.TemporaryDirectory() as temporary, mock.patch.dict(os.environ,GITHUB_STEP_SUMMARY=temporary+'/summary'):
                cmd.reset_mock(); promote.check('1.2.3'); cmd.assert_not_called()
                self.assertIn('registry incident',Path(temporary+'/summary').read_text())


class NpmPropagationTests(unittest.TestCase):
    PACKAGES = ('@skaft/octet-darwin-arm64', '@skaft/octet-darwin-x64',
                '@skaft/octet-linux-x64-gnu', '@skaft/octet-win32-x64', '@skaft/octet')

    def setUp(self):
        self.graph = jobs('release-octet.yml')[1]
        self.tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self.tmp.cleanup)
        self.root = Path(self.tmp.name)
        self.env = dict(os.environ, PATH=str(self.root)+os.pathsep+os.defpath,
                        VERSION='1.2.3', RUNNER_TEMP=str(self.root),
                        NPM_REGISTRY='https://registry.npmjs.org/', CALLS=str(self.root/'calls'))
        tool = self.root/'npm'
        tool.write_text(r'''#!/usr/bin/env python3
import json, os, pathlib, sys
root = pathlib.Path(os.environ['RUNNER_TEMP'])
a = sys.argv[1:]
with open(os.environ['CALLS'], 'a') as f: f.write(json.dumps(a)+'\n')
if a[0] == 'view':
    counter = root/('view-'+a[1].replace('/', '_'))
    count = int(counter.read_text())+1 if counter.exists() else 1
    counter.write_text(str(count))
    if count <= int(os.environ.get('MISSING_VIEWS', '0')):
        sys.exit(0 if os.environ.get('EMPTY_VIEW') else 1)
    print('sha512-visible')
elif a[0] == 'install':
    if os.environ.get('INSTALL_FAIL'): sys.exit(1)
    counter = root/'installs'
    count = int(counter.read_text())+1 if counter.exists() else 1
    counter.write_text(str(count))
    prefix = pathlib.Path(a[a.index('--prefix')+1])/'bin'
    prefix.mkdir(parents=True, exist_ok=True)
    binary = prefix/'octet'
    if count <= int(os.environ.get('MISSING_INSTALLS', '0')):
        binary.write_text('#!/bin/sh\necho "'+os.environ.get('LAUNCH_ERROR', 'missing the selected platform runtime: @skaft/octet-linux-x64-gnu')+'" >&2\nexit 1\n')
    else:
        binary.write_text('#!/bin/sh\necho "octet '+os.environ['VERSION']+'"\n')
    binary.chmod(0o755)
    host = prefix/'octet-host'
    host.write_text('#!/bin/sh\ncat >/dev/null\necho \'{"type":"hello"}\'\n')
    host.chmod(0o755)
elif a[0] == 'uninstall':
    prefix = pathlib.Path(a[a.index('--prefix')+1])/'bin'
    for p in prefix.iterdir(): p.unlink()
else: sys.exit('unexpected npm operation')
''')
        tool.chmod(0o755)

    def run_step(self, script):
        # Advance the actual shell deadline without wall-clock sleeps or networking.
        script = 'sleep() { SECONDS=$((SECONDS + $1)); }\n'+script
        return subprocess.run(['bash', '-eu', '-c', script], env=self.env,
                              cwd=self.root, text=True, capture_output=True, timeout=10)

    def calls(self):
        import json
        return [json.loads(line) for line in (self.root/'calls').read_text().splitlines()]

    def test_every_registry_reader_waits_with_shared_deadline_and_fetch_timeout(self):
        for job, next_step in (('publish-npm', 'Verify registry integrity and provenance'),
                               ('verify-npm-published', 'Verify public registry contents and provenance'),
                               ('smoke-npm-published', 'Install the exact public launcher without lifecycle scripts'),
                               ('smoke-npm-windows', 'Install exact npm version without lifecycle scripts')):
            with self.subTest(job=job):
                block = self.graph[job]
                wait = step(block, 'Wait for npm registry propagation')
                self.assertLess(block.index('Wait for npm registry propagation'), block.index(next_step))
                self.assertIn('timeout-minutes: 16', block)
                self.assertEqual(step(self.graph['publish-npm'], 'Wait for npm registry propagation'), wait)
                self.assertIn('deadline=$((SECONDS + 900))', wait)
                self.assertLess(wait.index('deadline='), wait.index('for package'))
                self.assertIn('SECONDS < deadline', wait)
                self.assertIn('sleep 15', wait)
                self.assertIn('dist.integrity', wait)
                self.assertIn('--fetch-retries=0 --fetch-timeout=10000', wait)
                for package in self.PACKAGES: self.assertIn(package, wait)
                if job in ('publish-npm', 'verify-npm-published'):
                    verify = step(block, next_step)
                    self.assertIn('verify-octet-npm-provenance.py', verify)
                    self.assertIn('"$expected"', verify)
                    self.assertNotIn('continue-on-error', block)

    def test_wait_retries_404_and_empty_integrity_until_all_five_versions_are_visible(self):
        script = step(self.graph['publish-npm'], 'Wait for npm registry propagation')
        self.env['MISSING_VIEWS'] = '2'
        for empty in ('', '1'):
            with self.subTest(empty=bool(empty)):
                self.env['EMPTY_VIEW'] = empty
                for path in self.root.glob('view-*'): path.unlink()
                (self.root/'calls').unlink(missing_ok=True)
                result = self.run_step(script)
                self.assertEqual(0, result.returncode, result.stdout+result.stderr)
                self.assertEqual([package+'@1.2.3' for package in self.PACKAGES for _ in range(3)],
                                 [call[1] for call in self.calls()])
                self.assertTrue(all(call[2] == 'dist.integrity' for call in self.calls()))

    def test_wait_expires_and_names_the_missing_package_and_version(self):
        script = step(self.graph['publish-npm'], 'Wait for npm registry propagation')
        self.env['MISSING_VIEWS'] = '100'
        result = self.run_step(script)
        self.assertNotEqual(0, result.returncode)
        self.assertIn('npm registry propagation timed out after 900s', result.stderr)
        self.assertIn('@skaft/octet-darwin-arm64@1.2.3', result.stderr)
        self.assertEqual(60, len(self.calls()))

    def test_posix_install_retries_missing_runtime_with_uninstall_before_reinstall(self):
        script = step(self.graph['smoke-npm-published'], 'Install the exact public launcher without lifecycle scripts')
        self.env['MISSING_INSTALLS'] = '1'
        result = self.run_step(script)
        self.assertEqual(0, result.returncode, result.stdout+result.stderr)
        self.assertEqual(['install', 'uninstall', 'install', 'uninstall'], [call[0] for call in self.calls()])
        for call in self.calls():
            self.assertIn('--ignore-scripts', call)
        self.assertIn('--prefer-online', self.calls()[0])
        self.assertFalse((self.root/'octet-npm-prefix/bin/octet').exists())

    def test_posix_install_stops_after_three_missing_runtimes(self):
        script = step(self.graph['smoke-npm-published'], 'Install the exact public launcher without lifecycle scripts')
        self.env['MISSING_INSTALLS'] = '10'
        result = self.run_step(script)
        self.assertNotEqual(0, result.returncode)
        self.assertIn('after 3 install attempts', result.stderr)
        self.assertEqual(['install', 'uninstall', 'install', 'uninstall', 'install'], [c[0] for c in self.calls()])

    def test_posix_install_does_not_retry_unrelated_failures(self):
        script = step(self.graph['smoke-npm-published'], 'Install the exact public launcher without lifecycle scripts')
        self.env.update(MISSING_INSTALLS='1', LAUNCH_ERROR='platform version does not match launcher version')
        result = self.run_step(script)
        self.assertNotEqual(0, result.returncode)
        self.assertIn(self.env['LAUNCH_ERROR'], result.stderr)
        self.assertEqual(['install'], [c[0] for c in self.calls()])

    def test_smoke_install_deadlines_and_windows_retry_contract(self):
        for job in ('smoke-npm-published', 'smoke-npm-windows'):
            self.assertIn('timeout-minutes: 10', self.graph[job])
        script = step(self.graph['smoke-npm-windows'], 'Install exact npm version without lifecycle scripts')
        self.assertIn('$attempt -le 3', script)
        self.assertIn('$attempt -eq 3', script)
        self.assertIn('missing the selected platform runtime:', script)
        self.assertIn('Select-String -SimpleMatch -Quiet', script)
        self.assertIn('Start-Sleep -Seconds 15', script)
        self.assertIn('npm uninstall --global', script)
        self.assertIn('--prefer-online', script)
        self.assertIn('$LASTEXITCODE -ne 0', script)
        self.assertLess(script.index('npm uninstall --global'), script.index('Start-Sleep'))
        self.assertIn('Windows npm help failed', script)
        self.assertIn('Windows npm host failed', script)


if __name__ == '__main__':
    unittest.main()
