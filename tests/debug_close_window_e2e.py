#!/usr/bin/env python3
"""Opt-in native ART Debug/Cancel/confirmed-close gate on one explicit transport."""
from pathlib import Path
import argparse, hashlib, json, os, shutil, struct, subprocess, sys
from adb_direct import Adb
from native_window_process import run_window, focus_callback


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument('serial')
    ap.add_argument('fixture', type=Path)
    ap.add_argument('output', type=Path)
    ap.add_argument('--mode', choices=['physical', 'virtual'], default='physical')
    ap.add_argument('--expected-hardware-serial')
    ap.add_argument('--reduced', action='store_true')
    args = ap.parse_args()
    root = Path(__file__).resolve().parents[1]
    out = args.output.resolve()
    binary = Path(os.environ.get('WEX_BIN', root/'target/debug/wex')).resolve()
    ide = Path(os.environ.get('WEX_IDE', root/'target/debug/wex-ide')).resolve()
    # Identity precedes tools, copying and installation. Never select another device.
    devices = json.loads(subprocess.check_output([binary, 'devices']))
    assert any(d['serial'] == args.serial and d['state'] == 'device' for d in devices)
    adb = Adb(args.serial)
    props = {k: adb.shell('getprop', k)['stdout'].strip() for k in
             ['ro.kernel.qemu', 'ro.serialno', 'ro.boot.serialno', 'ro.product.model']}
    assert (props['ro.kernel.qemu'] == '1') == (args.mode == 'virtual'), props
    if args.mode == 'physical':
        expected = args.expected_hardware_serial or args.serial
        assert props['ro.serialno'] == expected and props['ro.boot.serialno'] == expected
    out.mkdir(parents=True, exist_ok=False)
    (out/'identity.json').write_text(json.dumps({'serial': args.serial, 'mode': args.mode,
        'properties': props, 'devices': devices}, indent=2)+'\n')
    project = out/'project'
    shutil.copytree(args.fixture.resolve(), project,
                    ignore=shutil.ignore_patterns('.wex', '.git', '.gradle', 'build'))
    config = json.loads((project/'wex.json').read_text())
    application = 'dev.wex.debugclose.t'+hashlib.sha256(os.fsencode(out)).hexdigest()[:16]
    config['android']['application_id'] = application
    (project/'wex.json').write_text(json.dumps(config, indent=2)+'\n')
    assert not list(project.rglob('gradlew')) and not list(project.rglob('*.gradle*'))
    assert not adb.shell('pm', 'list', 'packages', application)['stdout'].strip()
    sources = {str(p): hashlib.sha256(p.read_bytes()).hexdigest() for p in project.rglob('*')
               if p.suffix in ['.kt', '.java'] and '.wex' not in p.parts}
    prefs = project/'.wex/breakpoints.json'
    pref_before = prefs.read_bytes() if prefs.exists() else None

    def settings():
        lines = adb.shell('settings', 'list', 'global')['stdout'].splitlines()
        return {k: [s for s in lines if s.startswith(k+'=')]
                for k in ['debug_app', 'wait_for_debugger']}

    before = settings()
    (out/'before.json').write_text(json.dumps({'sources': sources, 'settings': before,
        'application': application, 'prefs': hashlib.sha256(pref_before).hexdigest()
        if pref_before is not None else None}, indent=2)+'\n')
    traps = out/'traps'; traps.mkdir()
    for name in ['gradle', 'gradlew', 'mvn', 'bridge']:
        p = traps/name
        p.write_text('#!/bin/sh\nprintf forbidden > "'+str(out/'FORBIDDEN_EXECUTED')+'"\nexit 99\n')
        p.chmod(0o755)
    env = {**os.environ, 'WEX_TOOLS_HOME': str(root/'.wex'),
        'JAVA_HOME': '/usr/lib/jvm/java-21-openjdk', 'ANDROID_HOME': '/home/zapahtravki/Android/Sdk',
        'WEX_DEBUG_JAVA_HOME': '/usr/lib/jvm/java-21-openjdk',
        'WEX_DEBUG_EXPECT_EMULATOR': '1' if args.mode == 'virtual' else '0',
        'WEX_K2_SYSTEM_DIR': str(out/'k2-system'), 'WEX_PROJECT_HISTORY': str(out/'history.json'),
        'WEX_K2_SHUTDOWN_REPORT': str(out/'shutdown.json'),
        'IJ_JAVA_OPTIONS': '-XX:ActiveProcessorCount=2 -Xmx1024m',
        'WEX_LSP_TRACE': str(out/'lsp.jsonl'), 'PATH': str(traps)+os.pathsep+os.environ['PATH']}
    for k in ['WAYLAND_DISPLAY', 'WEX_REDUCED_MOTION']: env.pop(k, None)
    if args.reduced: env['WEX_REDUCED_MOTION'] = '1'

    def invoke(label, command, timeout=300, gui=False, json_stdout=False):
        e = {**env, 'WEX_DAP_TRACE': str(out/(label+'-dap.jsonl'))}
        code = run_window(command, e, out/(label+'.json' if json_stdout else label+'.log'),
            out/(label+'-process.json'), timeout,
            **({'on_start': focus_callback(command, out, timeout)} if gui else {}),
            **({'stderr_path': out/(label+'.log')} if json_stdout else {}))
        (out/(label+'.exit')).write_text(str(code)+'\n')
        assert code == 0, (label, code, (out/(label+'.log')).read_text()[-16000:])

    success = False
    try:
        c = subprocess.run([binary, 'resolve', project, '--offline'], env=env,
                           capture_output=True, text=True, timeout=180)
        (out/'resolve.log').write_text(c.stdout+c.stderr); assert c.returncode == 0, c.stderr
        invoke('cli', [binary, 'debug-check', project, '--serial', args.serial,
                       '--output', out/'cli-debug.json'])
        invoke('gui', [ide, project, '--debug-close-smoke', out, '--serial', args.serial], gui=True)
        # The complete original Debug validator stays strict: 13 original captures,
        # actual values/100+100+50 pages, complete stacks, stop/PID and raw DAP shapes.
        code = (root/'scripts/check-debug.sh').read_text().split(
            'python3 - "$PROJECT" "$OUT" "$SERIAL" <<\'PY\'\n', 1)[1].split('\nPY\n', 1)[0]
        c = subprocess.run([sys.executable, '-c', code, project, out, args.serial], env=env,
                           capture_output=True, text=True, timeout=30)
        (out/'validation.log').write_text(c.stdout+c.stderr); assert c.returncode == 0, c.stdout+c.stderr
        close = json.loads((out/'debug-close-window.json').read_text())
        exit_proof = json.loads((out/'debug-close-exit.json').read_text())
        run = json.loads((out/'debug-window.json').read_text())['run']
        assert close['result'] == 'GUI_ANDROID_DEBUG_CLOSE_OK' and close['serial'] == args.serial
        assert close['pid'] == run['pid'] == exit_proof['pid']
        assert close['run_id'] == run['run_id'] == exit_proof['run_id']
        assert close['before'] == close['after_cancel']
        assert close['fresh_pause']['epoch'] > close['cancel_epoch']
        assert close['original_deadline_seconds'] == 240 and close['original_step_bound_seconds'] == 20
        assert close['reduced_motion'] == args.reduced == exit_proof['reduced_motion']
        for snapshot in [close['before'], close['after_cancel'], close['fresh_pause']]:
            assert snapshot['frames'] and not snapshot['stack_loading'] and snapshot['stack_error'] is None
            assert not snapshot['scopes_loading'] and snapshot['scopes_error'] is None
            assert snapshot['variables_loading'] == [] and snapshot['variable_errors'] == {}
            assert all(s['variablesReference'] == 0 or str(s['variablesReference']) in snapshot['variables']
                       for s in snapshot['scopes'])
        assert len(set(close['cancel_frames'])) >= 4 and len(set(close['confirm_frames'])) >= 4
        assert len(close['screenshots']) == 3
        for shot in close['screenshots']:
            b = Path(shot['path']).read_bytes()
            assert b[:8] == b'\x89PNG\r\n\x1a\n' and struct.unpack('>II', b[16:24]) == (760, 560)
        assert exit_proof['closed_via_transaction'] and exit_proof['source_clean']
        assert 'WINDOW_CLOSE_COMMITTED source_save=true discarded_markers=false backup=none' == exit_proof['audit']
        assert close['stop']['pid_gone'] and exit_proof['stop']['pid_gone']
        requests = [json.loads(l.split(' ', 1)[1]) for l in (out/'gui-dap.jsonl').read_text().splitlines()
                    if l.startswith('send ')]
        assert sum(x.get('command') == 'disconnect' for x in requests) == 1
        assert sum(x.get('command') == 'pause' for x in requests) >= 2
        assert sum(x.get('command') == 'continue' for x in requests) >= 3
        assert not adb.shell('pidof', application, check=False)['stdout'].strip()
        assert settings() == before
        assert (prefs.read_bytes() if prefs.exists() else None) == pref_before
        assert all(hashlib.sha256(Path(p).read_bytes()).hexdigest() == h for p, h in sources.items())
        assert json.loads((out/'shutdown.json').read_text()) == {'pending': 0, 'forced': 0, 'spawn_failed': 0}
        assert not json.loads((out/'gui-process.json').read_text())['remaining']
        invoke('normal-run', [binary, 'run', project, '--serial', args.serial, '--offline',
            '--log-seconds', '4', '--require-marker', 'WEX_RUN_OK'], 240, json_stdout=True)
        normal = json.loads((out/'normal-run.json').read_text())
        assert normal['result'] == 'RUN_OK' and normal['logcat']['marker_found']
        assert settings() == before and not (out/'FORBIDDEN_EXECUTED').exists()
        (out/'proof.json').write_text(json.dumps({'result': 'ACTUAL_ART_REVIEWED_CLOSE_OK',
            'serial': args.serial, 'mode': args.mode, 'reduced_motion': args.reduced,
            'binary_sha256': hashlib.sha256(ide.read_bytes()).hexdigest(),
            'close': close, 'exit': exit_proof, 'normal_run': normal,
            'source_unchanged': True, 'settings_restored': True}, indent=2)+'\n')
        success = True
    finally:
        # The generated package was absent at preflight. Never uninstall a user's app.
        adb.shell('am', 'force-stop', '--user', 'current', application, check=False)
        installed = adb.shell('pm', 'list', 'packages', application)['stdout'].strip()
        removed = adb.shell('pm', 'uninstall', application) if installed else None
        cleanup = {'serial': args.serial, 'owned_package': application,
            'removed': not adb.shell('pm', 'list', 'packages', application)['stdout'].strip(),
            'pid_gone': not adb.shell('pidof', application, check=False)['stdout'].strip(),
            'settings_before': before, 'settings_after': settings(), 'uninstall': removed}
        (out/'cleanup.json').write_text(json.dumps(cleanup, indent=2)+'\n')
        if success: assert cleanup['removed'] and cleanup['pid_gone'] and cleanup['settings_after'] == before
    print('ACTUAL_ART_REVIEWED_CLOSE_OK serial='+args.serial+' reduced='+str(args.reduced)+
          ' native_captures=16 normal_Run_Logcat=true original_sources=true owned_package_removed=true', flush=True)


if __name__ == '__main__': main()
