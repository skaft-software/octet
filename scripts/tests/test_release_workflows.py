"""No-network contracts for the candidate/sign/publish dependency graph and policy."""
import hashlib
from importlib import import_module
import os
from pathlib import Path
import re
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


def step(text, name):
    block = text.split('      - name: '+name+'\n',1)[1]
    return textwrap.dedent(block.split('        run: |\n',1)[1].split('\n      - ',1)[0]).rstrip()


class WorkflowContracts(unittest.TestCase):
    def test_no_push_trigger_and_no_runner_workflow_polling(self):
        for name in ('release-candidate.yml','release-octet.yml','release-serve.yml'):
            text, graph = jobs(name)
            self.assertNotRegex(text.split('\njobs:\n',1)[0],r'(?m)^  push:')
            self.assertNotRegex(graph['resolve'],r'\bsleep\b|for attempt|while |run watch')
            self.assertNotIn('timeout-minutes: 85',text)
            self.assertNotIn('timeout-minutes: 360',text)
        self.assertNotIn('release-octet.yml/runs',jobs('release-serve.yml')[1]['resolve'])

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
        # The extension client has no certificate regex. Don't invent one or
        # weaken its separate HTTPS/checksum/trust boundary to satisfy a test.
        client=(ROOT/'crates/octet-coding-agent/src/extension_bundle.rs').read_text()
        self.assertIn('checksum_for_asset(&checksums, &asset)?',client)
        self.assertIn('if actual != expected',client)

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


if __name__ == '__main__':
    unittest.main()
