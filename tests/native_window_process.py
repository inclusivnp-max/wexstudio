#!/usr/bin/env python3
"""Run one owned native-window session and reap its background test services.

Only the session created by this Popen is eligible. Neither process names nor
window titles establish ownership. This does not manage deliberately detached
sessions or any already running application.
"""
from pathlib import Path
from contextlib import ExitStack
import json
import os
import signal
import subprocess
import time


def _state(pid):
    try:
        text = Path('/proc', str(pid), 'stat').read_text()
        fields = text[text.rindex(')') + 2:].split()
        return {'pid': pid, 'session': int(fields[3]),
                'start': int(fields[19]), 'state': fields[0]}
    except (OSError, ValueError, IndexError):
        return None


def _members(session, start):
    result = []
    for p in Path('/proc').iterdir():
        if p.name.isdigit():
            st = _state(int(p.name))
            if (st and st['session'] == session and st['start'] >= start
                    and st['state'] != 'Z'):
                result.append(st)
    return result


def _signal(st, sig):
    # Bind the signal to the inspected process, not a potentially reused PID.
    try:
        fd = os.pidfd_open(st['pid'])
    except ProcessLookupError:
        return False
    try:
        now = _state(st['pid'])
        if not now or any(now[k] != st[k] for k in ('pid', 'session', 'start')):
            return False
        signal.pidfd_send_signal(fd, sig)
        return True
    except ProcessLookupError:
        return False
    finally:
        os.close(fd)


def run_window(command, env, log_path, receipt_path, timeout, *, on_start=None,
               stderr_path=None):
    """Return the actual exit code; preserve timeout/error and cleanup receipts."""
    command = [str(x) for x in command]
    receipt_path = Path(receipt_path)
    with ExitStack() as stack:
        log = stack.enter_context(Path(log_path).open('w'))
        err = (stack.enter_context(Path(stderr_path).open('w'))
               if stderr_path is not None else subprocess.STDOUT)
        proc = subprocess.Popen(command, env=env, stdout=log,
                                stderr=err, start_new_session=True)
        owned = _state(proc.pid)
        assert owned and owned['session'] == proc.pid != os.getsid(0)
        receipt = {'pid': proc.pid, 'session': proc.pid, 'start': owned['start'],
                   'command': command, 'signals': []}
        receipt_path.write_text(json.dumps(receipt, indent=2) + '\n')
        try:
            # Focus/ownership setup is part of the protected lifetime too: an
            # exception here must not leave the launched IDE/JVM behind.
            if on_start is not None:
                on_start(proc)
            code = proc.wait(timeout=timeout)
            receipt['exit'] = code
            return code
        except BaseException as error:
            receipt['error'] = type(error).__name__
            raise
        finally:
            # A normal IDE exit can leave its JVM alive too. This exact session
            # includes owned Git process groups, without touching the user's IDE.
            for sig in (signal.SIGTERM, signal.SIGKILL):
                members = _members(proc.pid, owned['start'])
                if not members:
                    break
                for st in members:
                    if _signal(st, sig):
                        receipt['signals'].append({'pid': st['pid'],
                                                   'signal': sig.name})
                deadline = time.monotonic() + 3
                while time.monotonic() < deadline:
                    proc.poll()  # Reap the direct child, including timeout exits.
                    if not _members(proc.pid, owned['start']):
                        break
                    time.sleep(.05)
            if proc.poll() is None:
                proc.wait(timeout=3)
            receipt['remaining'] = _members(proc.pid, owned['start'])
            receipt_path.write_text(json.dumps(receipt, indent=2) + '\n')
            if receipt['remaining']:
                raise RuntimeError('Owned native-window session did not stop')


def focus_callback(command, output, timeout):
 expected=list(map(str,command));out=Path(output)
 def focus(proc):
  state=_state(proc.pid);assert state and state['session']==proc.pid
  deadline=time.monotonic()+15;observations=[]
  while proc.poll()is None and time.monotonic()<deadline:
   now=_state(proc.pid);assert now and all(now[k]==state[k]for k in ['pid','session','start'])
   actual=Path('/proc',str(proc.pid),'cmdline').read_bytes().split(b'\0')[:-1]
   if actual!=[os.fsencode(x)for x in expected]:time.sleep(.05);continue
   ids=subprocess.run(['xdotool','search','--onlyvisible','--pid',str(proc.pid)],capture_output=True,text=True).stdout.split()
   for xid in ids:
    prop=subprocess.run(['xprop','-id',xid,'_NET_WM_PID'],capture_output=True,text=True)
    if prop.stdout.strip()!=f'_NET_WM_PID(CARDINAL) = {proc.pid}':continue
    now=_state(proc.pid);assert now and all(now[k]==state[k]for k in ['pid','session','start'])
    result=subprocess.run(['xdotool','windowfocus','--sync',xid],capture_output=True,text=True,timeout=5);observed=subprocess.run(['xdotool','getwindowfocus'],capture_output=True,text=True)
    data={'pid':proc.pid,'start':state['start'],'expected_argv':expected,'actual_argv':[os.fsdecode(x)for x in actual],'xid':xid,'property':prop.stdout.strip(),'focus_exit':result.returncode,'observed_focus':observed.stdout.strip(),'observed_exit':observed.returncode,'setup_deadline_seconds':15,'native_tape_timeout_unchanged':timeout};observations.append(data)
    if result.returncode==observed.returncode==0 and observed.stdout.strip()==xid:
     (out/'owned-focus.json').write_text(json.dumps(data,indent=2)+'\n');return
   time.sleep(.2)
  (out/'focus-observations.json').write_text(json.dumps(observations,indent=2)+'\n');raise AssertionError('Exact owned native PID/argv/X11 focus was not established')
 return focus


if __name__ == '__main__':
    import sys
    import tempfile
    import unittest

    class OwnedWindowTests(unittest.TestCase):
        def check_exit(self, timeout):
            with tempfile.TemporaryDirectory(prefix='wex-process-guard-') as tmp:
                root = Path(tmp)
                sentinel = subprocess.Popen(
                    [sys.executable, '-c', 'import time;time.sleep(30)'],
                    start_new_session=True)
                self.addCleanup(lambda: sentinel.poll() is None and sentinel.kill())
                child = root / 'child.json'
                script = (
                    'import subprocess,sys,time,json,os,pathlib;'
                    'p=subprocess.Popen([sys.executable,"-c",'
                    '"import signal,time;signal.signal(signal.SIGTERM,signal.SIG_IGN);'
                    'time.sleep(30)"],process_group=0);'
                    'pathlib.Path(sys.argv[1]).write_text(json.dumps({"pid":p.pid,"sid":os.getsid(p.pid)}));'
                    'time.sleep(30)' if timeout else
                    'import subprocess,sys,time,json,os,pathlib;'
                    'p=subprocess.Popen([sys.executable,"-c","import time;time.sleep(30)"],process_group=0);'
                    'pathlib.Path(sys.argv[1]).write_text(json.dumps({"pid":p.pid,"sid":os.getsid(p.pid)}));'
                    'time.sleep(.1)')
                try:
                    args = ([sys.executable, '-c', script, str(child)],
                            os.environ.copy(), root / 'window.log',
                            root / 'process.json', .5 if timeout else 5)
                    if timeout:
                        with self.assertRaises(subprocess.TimeoutExpired):
                            run_window(*args)
                    else:
                        self.assertEqual(run_window(*args), 0)
                    receipt = json.loads((root / 'process.json').read_text())
                    background = json.loads(child.read_text())
                    self.assertNotEqual(background['pid'], receipt['session'])
                    self.assertEqual(background['sid'], receipt['session'])
                    self.assertEqual(receipt['remaining'], [])
                    st = _state(background['pid'])
                    self.assertTrue(st is None or st['state'] == 'Z')
                    self.assertIsNone(sentinel.poll())
                    self.assertTrue(receipt['signals'])
                    if timeout:
                        self.assertEqual(receipt['error'], 'TimeoutExpired')
                finally:
                    sentinel.terminate()
                    sentinel.wait(timeout=5)

        def test_normal_exit_reaps_owned_background_not_other_session(self):
            self.check_exit(False)

        def test_timeout_preserved_and_owned_process_groups_stopped(self):
            self.check_exit(True)

        def test_start_callback_error_preserved_and_owned_session_stopped(self):
            with tempfile.TemporaryDirectory(prefix='wex-process-guard-') as tmp:
                root = Path(tmp)
                seen = []
                def start(proc):
                    self.assertEqual(os.getsid(proc.pid), proc.pid)
                    seen.append(proc.pid)
                    raise ValueError('owned focus setup failed')
                with self.assertRaisesRegex(ValueError, 'owned focus setup failed'):
                    run_window([sys.executable, '-c', 'import time;time.sleep(30)'],
                               os.environ.copy(), root/'window.log', root/'process.json',
                               5, on_start=start)
                receipt = json.loads((root/'process.json').read_text())
                self.assertEqual(receipt['error'], 'ValueError')
                self.assertEqual(receipt['remaining'], [])
                self.assertEqual(seen, [receipt['pid']])
                self.assertIsNone(_state(receipt['pid']))

        def test_separate_stderr_keeps_cli_json_machine_readable(self):
            with tempfile.TemporaryDirectory(prefix='wex-process-guard-') as tmp:
                root = Path(tmp)
                code = run_window([sys.executable, '-c',
                    'import sys,json;print(json.dumps({"ok":True}));print("diagnostic",file=sys.stderr)'],
                    os.environ.copy(), root/'stdout.json', root/'process.json', 5,
                    stderr_path=root/'stderr.log')
                self.assertEqual(code, 0)
                self.assertEqual(json.loads((root/'stdout.json').read_text()), {'ok':True})
                self.assertEqual((root/'stderr.log').read_text(), 'diagnostic\n')
                self.assertEqual(json.loads((root/'process.json').read_text())['remaining'], [])

    unittest.main(verbosity=2)
