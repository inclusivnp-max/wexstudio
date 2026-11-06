#!/usr/bin/env python3
"""Black-box CLI tests using isolated TCP and Gradle-wrapper fixtures."""
import json
import os
from pathlib import Path
import socket
import struct
import subprocess
import tempfile
import threading
import unittest

ROOT = Path(__file__).resolve().parents[1]
BIN = Path(os.environ.get('WEX_BIN', ROOT / 'target/debug/wex'))

class CliTests(unittest.TestCase):
    def run_cli(self, *args):
        return subprocess.run([str(BIN), *map(str,args)], capture_output=True, text=True, timeout=15)

    def server(self, handler):
        listener = socket.socket()
        listener.bind(('127.0.0.1',0))
        listener.listen(1)
        listener.settimeout(5)
        address = '%s:%s' % listener.getsockname()
        failures = []
        def run():
            try:
                with listener:
                    connection,_ = listener.accept()
                    with connection:
                        connection.settimeout(5)
                        handler(connection)
            except Exception as e:
                failures.append(e)
        thread = threading.Thread(target=run, daemon=True)
        thread.start()
        return address,thread,failures

    @staticmethod
    def read_exact(connection,n):
        data = b''
        while len(data)<n:
            part = connection.recv(n-len(data))
            if not part:
                raise EOFError('truncated request')
            data += part
        return data

    def command(self,connection):
        return self.read_exact(connection,int(self.read_exact(connection,4),16)).decode()

    def test_java_formatter_offline_missing_is_explicit_and_does_not_download(self):
        with tempfile.TemporaryDirectory() as tmp:
            env={**os.environ,'WEX_TOOLS_HOME':str(Path(tmp)/'tools'),'https_proxy':'http://127.0.0.1:1'}
            x=subprocess.run([str(BIN),'prepare-java-formatter','--offline'],cwd=tmp,env=env,capture_output=True,text=True,timeout=15)
            self.assertNotEqual(x.returncode,0)
            self.assertIn('missing/damaged',x.stderr)
            self.assertFalse(list(Path(tmp).rglob('*.jar')))
            self.assertFalse(list(Path(tmp).rglob('*.class')))
            self.assertFalse(list(Path(tmp).rglob('*.apk')))

    def test_new_templates_create_only_native_project_files(self):
        with tempfile.TemporaryDirectory() as d:
            for template in ['compose','kotlin','java']:
                x=self.run_cli('new',d,'--name','App & "name"','--directory',template,'--package','dev.wex.'+template,'--template',template)
                self.assertEqual(x.returncode,0,x.stderr)
                result=json.loads(x.stdout);root=Path(result['root']);config=json.loads((root/'wex.json').read_text())
                self.assertEqual(config['android']['application_id'],'dev.wex.'+template)
                self.assertTrue(Path(result['entry']).is_file())
                self.assertFalse((root/'.wex').exists());self.assertFalse((root/'wex.lock.json').exists())
                self.assertFalse(list(root.rglob('*gradle*')))
                self.assertEqual('compose' in config['build'],template=='compose')

    def test_new_invalid_package_and_directory_leave_parent_empty(self):
        with tempfile.TemporaryDirectory() as d:
            for directory,package in [('../escaped','dev.wex.app'),('app','com.when'),('app','1bad.app')]:
                x=self.run_cli('new',d,'--name','App','--directory',directory,'--package',package)
                self.assertNotEqual(x.returncode,0)
                self.assertEqual(list(Path(d).iterdir()),[])

    def test_new_existing_destination_remains_unchanged(self):
        with tempfile.TemporaryDirectory() as d:
            root=Path(d)/'app';root.mkdir();(root/'sentinel').write_text('keep')
            x=self.run_cli('new',d,'--name','App','--directory','app','--package','dev.wex.app')
            self.assertNotEqual(x.returncode,0);self.assertEqual((root/'sentinel').read_text(),'keep')
            self.assertEqual([p.name for p in root.iterdir()],['sentinel'])

    def test_new_invalid_sdk_is_rejected_before_creation(self):
        with tempfile.TemporaryDirectory() as d:
            x=self.run_cli('new',d,'--name','App','--directory','app','--package','dev.wex.app','--min-sdk','99')
            self.assertNotEqual(x.returncode,0);self.assertEqual(list(Path(d).iterdir()),[])

    def test_recent_missing_read_is_empty_without_creating_state(self):
        with tempfile.TemporaryDirectory() as d:
            history=Path(d)/'state/history.json'
            x=self.run_cli('recent','--history',history)
            self.assertEqual(x.returncode,0,x.stderr);self.assertEqual(json.loads(x.stdout),[])
            self.assertFalse(history.parent.exists())

    def test_recent_record_dedup_and_remove_preserve_project_files(self):
        with tempfile.TemporaryDirectory() as d:
            x=self.run_cli('new',d,'--name','Recent app','--directory','app','--package','dev.wex.recent')
            self.assertEqual(x.returncode,0,x.stderr);root=Path(d)/'app';before=(root/'wex.json').read_bytes();history=Path(d)/'history.json'
            for _ in range(2):
                x=self.run_cli('recent','--history',history,'--add',root)
                self.assertEqual(x.returncode,0,x.stderr);self.assertEqual(len(json.loads(x.stdout)),1)
            x=self.run_cli('recent','--history',history,'--remove',root)
            self.assertEqual(x.returncode,0,x.stderr);self.assertEqual(json.loads(x.stdout),[])
            self.assertEqual((root/'wex.json').read_bytes(),before)

    def test_recent_corrupt_history_is_preserved(self):
        with tempfile.TemporaryDirectory() as d:
            history=Path(d)/'history.json';history.write_text('broken')
            x=self.run_cli('recent','--history',history)
            self.assertNotEqual(x.returncode,0);self.assertEqual(history.read_text(),'broken')

    def test_browse_lists_only_sorted_directories_and_explicit_hidden(self):
        with tempfile.TemporaryDirectory() as d:
            p=Path(d);(p/'Beta').mkdir();(p/'alpha').mkdir();(p/'.hidden').mkdir();(p/'file').write_text('x')
            x=self.run_cli('browse',p);self.assertEqual(x.returncode,0,x.stderr)
            self.assertEqual([f['name'] for f in json.loads(x.stdout)['folders']],['alpha','Beta'])
            x=self.run_cli('browse',p,'--hidden');self.assertEqual(x.returncode,0,x.stderr)
            self.assertEqual(len(json.loads(x.stdout)['folders']),3)

    def test_doctor_is_read_only_and_performs_no_downloads(self):
        x=self.run_cli('doctor');self.assertEqual(x.returncode,0,x.stderr)
        report=json.loads(x.stdout);self.assertEqual(report['schema'],1);self.assertFalse(report['downloads_performed']);self.assertFalse(report['external_tools_bundled']);self.assertEqual(report['frontend_budget_bytes'],20000000)

    def test_runtime_overrides_bind_relative_paths_to_caller(self):
        with tempfile.TemporaryDirectory() as d:
            env={**os.environ,'WEX_SUPPORT_HOME':'assets','WEX_TOOLS_HOME':'runtime-data'}
            x=subprocess.run([str(BIN),'doctor'],cwd=d,env=env,text=True,capture_output=True,timeout=15)
            self.assertEqual(x.returncode,0,x.stderr);p=json.loads(x.stdout)['runtime'];self.assertEqual(p['support'],str(Path(d)/'assets'));self.assertEqual(p['tools'],str(Path(d)/'runtime-data'));self.assertFalse((Path(d)/'runtime-data').exists())

    def test_packaged_support_hash_and_extra_file_validation(self):
        import hashlib
        with tempfile.TemporaryDirectory() as d:
            root=Path(d);(root/'asset.txt').write_text('original');manifest={'schema':1,'files':{'asset.txt':hashlib.sha256(b'original').hexdigest()}};(root/'support.json').write_text(json.dumps(manifest));env={**os.environ,'WEX_SUPPORT_HOME':d,'WEX_TOOLS_HOME':str(root/'data')}
            x=subprocess.run([str(BIN),'doctor','--require-support'],env=env,text=True,capture_output=True,timeout=15);self.assertEqual(x.returncode,0,x.stderr)
            (root/'asset.txt').write_text('modified');x=subprocess.run([str(BIN),'doctor','--require-support'],env=env,text=True,capture_output=True,timeout=15);self.assertNotEqual(x.returncode,0);self.assertFalse(json.loads(x.stdout)['support_verified'])
            (root/'asset.txt').write_text('original');(root/'extra').write_text('unexpected');x=subprocess.run([str(BIN),'doctor','--require-support'],env=env,text=True,capture_output=True,timeout=15);self.assertNotEqual(x.returncode,0)

    def test_offline_lsp_prepare_missing_archive_never_downloads(self):
        import hashlib
        with tempfile.TemporaryDirectory() as d:
            root=Path(d);(root/'scripts').mkdir();source=(ROOT/'scripts/install-kotlin-lsp.py').read_bytes();(root/'scripts/install-kotlin-lsp.py').write_bytes(source);(root/'support.json').write_text(json.dumps({'schema':1,'files':{'scripts/install-kotlin-lsp.py':hashlib.sha256(source).hexdigest()}}));env={**os.environ,'WEX_SUPPORT_HOME':d,'WEX_TOOLS_HOME':str(root/'data'),'https_proxy':'http://127.0.0.1:1'}
            x=subprocess.run([str(BIN),'prepare-lsp','--offline'],env=env,text=True,capture_output=True,timeout=15);self.assertNotEqual(x.returncode,0);self.assertIn('offline preparation performed no downloads',x.stderr);self.assertFalse(list((root/'data').rglob('*.download')))

    def test_devices_mock_tcp(self):
        def handle(c):
            self.assertEqual(self.command(c),'host:devices-l')
            payload = b'fixture device model:Pixel transport_id:1\nlocked unauthorized\n'
            for part in [b'OK', b'AY', ('%04X'%len(payload)).encode(), payload]:
                c.sendall(part)
        addr,thread,failures = self.server(handle)
        result = self.run_cli('--adb',addr,'devices')
        thread.join(6)
        self.assertFalse(failures)
        self.assertEqual(result.returncode,0,result.stderr)
        self.assertEqual([d['state'] for d in json.loads(result.stdout)],['device','unauthorized'])

    def test_logcat_binary_stream_and_regex(self):
        def frame(tag,message):
            payload = b'\x04'+tag+b'\0'+message+b'\0'
            return struct.pack('<HH6I',len(payload),28,123,456,100,42,0,1000)+payload
        def handle(c):
            self.assertEqual(self.command(c),'host:transport:fixture')
            c.sendall(b'OKAY')
            self.assertEqual(self.command(c),'exec:logcat -B -b main -b system -b crash')
            c.sendall(b'OKAY')
            for byte in frame(b'Wex',b'keep me')+frame(b'Other',b'ignore'):
                c.sendall(bytes([byte]))
        addr,thread,failures = self.server(handle)
        result = self.run_cli('--adb',addr,'logcat','--serial','fixture','--regex','keep','--pid','123')
        thread.join(6)
        self.assertFalse(failures)
        self.assertEqual(result.returncode,0,result.stderr)
        lines = [json.loads(line) for line in result.stdout.splitlines()]
        self.assertEqual(len(lines),1)
        self.assertEqual(lines[0]['message'],'keep me')

    def test_logcat_replay_retains_full_truncated_payload_and_rejects_partial_frame(self):
        with tempfile.TemporaryDirectory() as directory:
            payload=b'\x04Tag\0'+b'x'*(4068-5)
            header=struct.pack('<HHIIIIII',len(payload),28,12,13,100,42,0,1000)
            path=Path(directory)/'raw.bin';path.write_bytes(header+payload)
            result=self.run_cli('logcat-replay',path)
            self.assertEqual(result.returncode,0,result.stderr)
            report=json.loads(result.stdout);self.assertEqual(report['entries'],1);self.assertEqual(report['truncated_messages'],1)
            path.write_bytes(header+payload[:-1]);self.assertNotEqual(self.run_cli('logcat-replay',path).returncode,0)

    def test_inspect_kotlin(self):
        result = self.run_cli('inspect',ROOT/'examples/demo/app/src/main/kotlin/dev/wex/MainActivity.kt')
        self.assertEqual(result.returncode,0,result.stderr)
        self.assertFalse(json.loads(result.stdout)['syntax_errors'])

    def test_build_wrapper_arguments_and_cwd(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            wrapper = root/'gradlew'
            wrapper.write_text('#!/bin/sh\nprintf "%s\\n" "$PWD" "$@"\n')
            wrapper.chmod(0o755)
            result = self.run_cli('gradle-build',root,'--task',':library:assembleDebug')
            self.assertEqual(result.returncode,0,result.stderr)
            self.assertEqual(result.stdout.splitlines(),[str(root),':library:assembleDebug','--console=plain'])

    def test_failed_build_is_not_success(self):
        with tempfile.TemporaryDirectory() as directory:
            wrapper = Path(directory)/'gradlew'
            wrapper.write_text('#!/bin/sh\nexit 73\n')
            wrapper.chmod(0o755)
            result = self.run_cli('gradle-build',directory)
            self.assertNotEqual(result.returncode,0)
            self.assertIn('73',result.stderr)

    def test_missing_wrapper_and_task_options(self):
        with tempfile.TemporaryDirectory() as directory:
            result = self.run_cli('gradle-build',directory)
            self.assertNotEqual(result.returncode,0)
            self.assertIn('no Gradle wrapper',result.stderr)
            result = self.run_cli('gradle-build',directory,'--task=--init-script')
            self.assertNotEqual(result.returncode,0)
            self.assertIn('invalid Gradle task',result.stderr)

    def test_project_discovery_avoids_build_output(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            (root/'Main.kt').write_text('val x = 1')
            (root/'build').mkdir()
            (root/'build/Generated.kt').write_text('val x = 2')
            result = self.run_cli('project',directory)
            self.assertEqual(result.returncode,0,result.stderr)
            project = json.loads(result.stdout)
            self.assertEqual(project['files'],[str(root/'Main.kt')])
            self.assertIsNone(project['cached_gradle_graph'])

    def test_k2_probe_ambiguous_default_source_requires_explicit_file(self):
        with tempfile.TemporaryDirectory() as directory:
            root=Path(directory)
            for name in ['app','other']:
                (root/name).mkdir();(root/name/'MainActivity.kt').write_text('class MainActivity')
            result=self.run_cli('k2-check',root)
            self.assertEqual(result.returncode,1)
            self.assertIn('explicit --file',result.stderr)
    def test_k2_probe_rejects_a_source_outside_the_selected_project(self):
        with tempfile.TemporaryDirectory() as directory:
            root=Path(directory);(root/'project').mkdir();source=root/'Outside.kt';source.write_text('val x=1')
            result=self.run_cli('k2-check',root/'project','--file',source)
            self.assertEqual(result.returncode,1)
            self.assertIn('must belong to the selected project',result.stderr)

    def test_debug_unavailable_exact_serial_has_no_tools_build_or_other_device_commands(self):
        def handle(c):
            self.assertEqual(self.command(c), 'host:devices-l')
            payload = b'adb-47101FDAS0028Q-wifi device model:Pixel\nemulator-5580 device\n'
            c.sendall(b'OKAY' + ('%04X'%len(payload)).encode() + payload)
        addr,thread,failures=self.server(handle)
        with tempfile.TemporaryDirectory() as folder:
            receipt=Path(folder)/'debug.json'
            result=self.run_cli('--adb',addr,'debug-check',ROOT/'tests/fixtures/native-android','--serial','47101FDAS0028Q','--output',receipt)
            self.assertEqual(result.returncode,1,result.stderr)
            self.assertIn('47101FDAS0028Q',result.stderr)
            self.assertIn('state: absent',result.stderr)
            self.assertNotIn('Preparing',result.stderr)
            self.assertFalse(receipt.exists())
        thread.join(6);self.assertFalse(failures)

    def test_run_requires_explicit_selected_serial(self):
        result = self.run_cli('run', ROOT/'tests/fixtures/android')
        self.assertEqual(result.returncode, 2)
        self.assertIn('--serial', result.stderr)
        self.assertNotIn('RUN_OK', result.stdout)

    def test_run_unavailable_device_stops_before_gradle(self):
        def handle(c):
            self.assertEqual(self.command(c), 'host:devices-l')
            payload = b'other device model:Other\nchosen unauthorized\n'
            c.sendall(b'OKAY' + ('%04X'%len(payload)).encode() + payload)
        addr, thread, failures = self.server(handle)
        result = self.run_cli('--adb', addr, 'run', ROOT, '--serial', 'chosen')
        thread.join(6)
        self.assertFalse(failures)
        self.assertEqual(result.returncode, 1)
        self.assertIn('not connected/authorized', result.stderr)
        self.assertNotIn('RUN_OK', result.stdout)

    def test_install_stream_and_package_manager_receipt(self):
        with tempfile.TemporaryDirectory() as directory:
            apk = Path(directory)/'app.apk'
            payload = b'APK-fixture' * 15000
            apk.write_bytes(payload)
            def handle(c):
                self.assertEqual(self.command(c), 'host:transport:chosen')
                c.sendall(b'OKAY')
                self.assertEqual(self.command(c), 'shell,v2,raw:cmd package install -r -t --user current -S %s'%len(payload))
                c.sendall(b'OKAY')
                actual = b''
                while True:
                    channel, size = struct.unpack('<BI', self.read_exact(c,5))
                    data = self.read_exact(c,size)
                    if channel == 4:
                        self.assertEqual(size, 0)
                        break
                    self.assertEqual(channel, 0)
                    self.assertLessEqual(size, 65536)
                    actual += data
                self.assertEqual(actual, payload)
                c.sendall(struct.pack('<BI',1,8)+b'Success\n'+struct.pack('<BI',3,1)+b'\0')
            addr, thread, failures = self.server(handle)
            result = self.run_cli('--adb', addr, 'install', '--serial', 'chosen', apk)
            thread.join(6)
            self.assertFalse(failures)
            self.assertEqual(result.returncode, 0, result.stderr)
            receipt = json.loads(result.stdout)
            self.assertEqual(receipt['bytes'], len(payload))
            self.assertEqual(receipt['output']['stdout'], 'Success\n')
            self.assertEqual(receipt['output']['exit_code'], 0)

    def test_invalid_logcat_window_is_rejected_before_deployment(self):
        result = self.run_cli('run', ROOT, '--serial', 'chosen', '--log-seconds', '0')
        self.assertEqual(result.returncode, 1)
        self.assertIn('1..300', result.stderr)

    def test_native_resolve_local_repo_without_executables(self):
        with tempfile.TemporaryDirectory() as directory:
            root=Path(directory)
            repo=root/'repo'; artifact=repo/'dev/test/a/1'
            artifact.mkdir(parents=True)
            (artifact/'a-1.pom').write_text('<project><groupId>dev.test</groupId><artifactId>a</artifactId><version>1</version></project>')
            (artifact/'a-1.jar').write_bytes(b'JAR-fixture')
            config={'schema':1,'name':'native','repositories':[repo.as_uri()],'android':{'compile_sdk':36,'min_sdk':24,'target_sdk':36,'application_id':'dev.wex.native'},'modules':[{'name':'app','sources':['src'],'dependencies':['dev.test:a:1']}]}
            (root/'wex.json').write_text(json.dumps(config))
            args=[str(BIN),'resolve',str(root),'--cache',str(root/'cache')]
            result=subprocess.run(args,capture_output=True,text=True,timeout=15,env={**os.environ,'PATH':'/nonexistent'})
            self.assertEqual(result.returncode,0,result.stderr)
            self.assertEqual(len(json.loads(result.stdout)['artifacts']),1)
            result=subprocess.run(args+['--offline','--locked'],capture_output=True,text=True,timeout=15,env={**os.environ,'PATH':'/nonexistent'})
            self.assertEqual(result.returncode,0,result.stderr)
            self.assertEqual(len(json.loads(result.stdout)['files']),2)

    def test_build_does_not_provision_missing_gradle(self):
        with tempfile.TemporaryDirectory() as directory:
            root=Path(directory)
            props=root/'gradle/wrapper'; props.mkdir(parents=True)
            (props/'gradle-wrapper.properties').write_text('distributionUrl=https\\://services.gradle.org/distributions/gradle-0.0.0-wex-no-download-bin.zip')
            wrapper=root/'gradlew'
            wrapper.write_text('#!/bin/sh\ntouch SHOULD_NOT_RUN\n')
            wrapper.chmod(0o755)
            result=self.run_cli('gradle-build',root)
            self.assertEqual(result.returncode,1)
            self.assertIn('download is disabled',result.stderr)
            self.assertFalse((root/'SHOULD_NOT_RUN').exists())

    def test_native_build_never_falls_back_to_wrapper(self):
        with tempfile.TemporaryDirectory() as directory:
            root=Path(directory)
            wrapper=root/'gradlew'
            wrapper.write_text('#!/bin/sh\ntouch SHOULD_NOT_RUN\n');wrapper.chmod(0o755)
            result=self.run_cli('build',root,'--offline')
            self.assertEqual(result.returncode,1)
            self.assertFalse((root/'SHOULD_NOT_RUN').exists())

if __name__=='__main__':
    unittest.main(verbosity=2)
