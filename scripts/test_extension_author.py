"""Local package authoring boundary tests; never execute authored code."""
import json, os, pathlib, subprocess, sys, tempfile, tomllib, unittest, time
ROOT = pathlib.Path(__file__).resolve().parents[1]
SCRIPT = ROOT / 'scripts/extension-author.py'
SOURCE = ROOT / 'examples/extensions/python-single-file/extension.py'

class AuthorTests(unittest.TestCase):
    def run_author(self, source, output, *extra):
        return subprocess.run([sys.executable, str(SCRIPT), str(source), str(output), '--name', 'wait-tool', '--tool', 'wait', *map(str, extra)], capture_output=True, text=True)
    def test_package_toml_quotes_assets_and_source(self):
        with tempfile.TemporaryDirectory(dir="/private/tmp", prefix='quoted"雪\n') as tmp:
            root = pathlib.Path(tmp); skill = root/'SKILL.md'; skill.write_text('Help')
            asset = root/'dátá.json'; asset.write_text('{}'); out = root/'pkg"quoted'
            result = self.run_author(SOURCE, out, '--skill', skill, '--asset', asset)
            self.assertEqual(result.returncode, 0, result.stderr)
            manifest_text = (out/'extension.toml').read_text()
            self.assertNotIn(str(out), manifest_text)
            manifest = tomllib.loads(manifest_text)
            self.assertEqual(manifest['entrypoint'], {'command':'run-extension','args':[]})
            host_version = tomllib.loads((ROOT/'Cargo.toml').read_text())['workspace']['package']['version']
            self.assertEqual(manifest['requires_octet'], f'={host_version}')
            self.assertTrue((out/'run-extension').stat().st_mode & 0o111)
            launcher = (out/'run-extension').read_text()
            self.assertIn('OCTET_EXTENSION_DIR', launcher)
            self.assertIn('python3 -B', launcher)
            self.assertNotIn(str(ROOT), launcher)
            sdk_root = ROOT/'sdk/python/octet_extension'
            expected_sdk = {path.relative_to(sdk_root) for path in sdk_root.rglob('*.py')}
            vendor_root = out/'vendor/octet_extension'
            packaged_sdk = {path.relative_to(vendor_root) for path in vendor_root.rglob('*.py')}
            self.assertEqual(packaged_sdk, expected_sdk)
            self.assertFalse(list(out.rglob('__pycache__')))
            self.assertEqual(manifest['contributes']['tools'], ['wait'])
            self.assertEqual((out/'SKILL.md').read_text(), 'Help')
            self.assertEqual((out/'assets/dátá.json').read_text(), '{}')
            provenance = (out/'SDK-PROVENANCE.txt').read_text()
            sdk_version = tomllib.loads((ROOT/'sdk/python/pyproject.toml').read_text())['project']['version']
            self.assertIn(f'octet-extension-sdk {sdk_version}', provenance)
            self.assertIn('SDK-LICENSE', provenance)
    def test_source_and_asset_symlinks_refused_without_output(self):
        with tempfile.TemporaryDirectory(dir="/private/tmp") as tmp:
            root = pathlib.Path(tmp); src = root/'source.py'; src.write_text(SOURCE.read_text())
            link = root/'source-link.py'; link.symlink_to(src)
            self.assertNotEqual(self.run_author(link, root/'a').returncode, 0)
            self.assertFalse((root/'a').exists())
            traversal = root/'sub'/'..'/'source.py'
            self.assertNotEqual(self.run_author(traversal, root/'traversal').returncode, 0)
            self.assertFalse((root/'traversal').exists())
            asset = root/'secret'; asset.write_text('secret'); linkasset = root/'asset-link'; linkasset.symlink_to(asset)
            result = self.run_author(SOURCE, root/'b', '--asset', linkasset)
            self.assertNotEqual(result.returncode, 0); self.assertFalse((root/'b').exists())
    def test_bad_declarations_bounds_and_collisions_fail_before_creation(self):
        with tempfile.TemporaryDirectory(dir="/private/tmp") as tmp:
            root=pathlib.Path(tmp); output=root/'out'
            for text in ['@ext.tool(name=NAME, description="x")\ndef f(a): pass\n',
                '@ext.tool(name="wait", description="x")\ndef f(a): pass\n@ext.tool(name="wait", description="x")\ndef g(a): pass\n',
                '@ext.tool(name="wait", name="wait", description="x")\ndef f(a): pass\n',
                '@ext.tool(name="wait", description=DESC)\ndef f(a): pass\n',
                'x'*1_000_001]:
                source=root/'candidate.py'; source.write_text(text)
                result=self.run_author(source, output)
                self.assertNotEqual(result.returncode, 0); self.assertFalse(output.exists())
            src=root/'ok.py'; src.write_text('@ext.tool(description="Wait", name="wait")\ndef f(a): return "ok"\n')
            self.assertEqual(self.run_author(src, root/'ordered').returncode, 0)
            src=root/'ok.py'; src.write_text(SOURCE.read_text())
            a=root/'same'; b=root/'same'; a.write_text('a')
            result=self.run_author(src, output, '--asset', a, '--asset', b)
            self.assertNotEqual(result.returncode, 0); self.assertFalse(output.exists())
            control=root/'bad\nname'; control.write_text('x')
            result=self.run_author(src, root/'control-output', '--asset', control)
            self.assertNotEqual(result.returncode, 0); self.assertFalse((root/'control-output').exists())
            large=root/'large'; large.write_bytes(b'x'*10_000_001)
            result=self.run_author(src, root/'oversize', '--asset', large)
            self.assertNotEqual(result.returncode, 0); self.assertFalse((root/'oversize').exists())
    def test_generated_package_process_initialize_cancel_shutdown(self):
        with tempfile.TemporaryDirectory(dir="/private/tmp") as tmp:
            root=pathlib.Path(tmp); package=root/'pkg'
            result=self.run_author(SOURCE, package)
            self.assertEqual(result.returncode, 0, result.stderr)
            env={key:value for key,value in os.environ.items() if key != 'PYTHONPATH'}
            process=subprocess.Popen(['python3', str(package/'extension.py')], stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True, env=env, bufsize=1)
            def send(value):
                process.stdin.write(json.dumps(value)+'\n'); process.stdin.flush()
            def receive(): return json.loads(process.stdout.readline())
            send({'jsonrpc':'2.0','id':1,'method':'initialize','params':{'api_version':'0.4','contributes':{'tools':['wait']},'protocol':{'version':'0.4','required_features':['request_cancellation','content_parts'],'optional_features':[],'limits':{'max_concurrent_requests':1}}}})
            self.assertEqual(receive()['id'],1)
            send({'jsonrpc':'2.0','id':2,'method':'tool/call','params':{'name':'wait','arguments':{'steps':100},'context':{}}})
            time.sleep(0.15)
            send({'jsonrpc':'2.0','method':'$/cancelRequest','params':{'id':2,'reason':'test cancellation'}})
            response=receive(); self.assertEqual(response['id'],2); self.assertIn('error',response)
            send({'jsonrpc':'2.0','id':3,'method':'shutdown','params':{}})
            self.assertEqual(receive()['id'],3)
            process.stdin.close(); self.assertEqual(process.wait(timeout=5),0, process.stderr.read())
            process.stdout.close(); process.stderr.close()
    def test_duplicate_invalid_tool_arguments_refused(self):
        with tempfile.TemporaryDirectory(dir="/private/tmp") as tmp:
            out=pathlib.Path(tmp)/'out'
            result=subprocess.run([sys.executable,str(SCRIPT),str(SOURCE),str(out),'--name','ok','--tool','bad-name'],capture_output=True,text=True)
            self.assertNotEqual(result.returncode,0); self.assertFalse(out.exists())

if __name__ == '__main__': unittest.main()
