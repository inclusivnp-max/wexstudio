#!/usr/bin/env python3
"""Real native terminal: controlling PTY, bash/readline, curses, resize, process cleanup."""
from pathlib import Path
import hashlib,json,os,shutil,struct,subprocess,sys,time,signal
R=Path(__file__).resolve().parents[1];O=Path(sys.argv[1]).resolve();O.mkdir(parents=True,exist_ok=False);P=O/'project';W=O/'window';W.mkdir()
shutil.copytree(R/'tests/fixtures/native-android',P,ignore=shutil.ignore_patterns('.wex'))
(P/'terminal_tui.py').write_text('''import curses
from pathlib import Path
def app(s):
 curses.start_color();curses.init_pair(1,curses.COLOR_MAGENTA,curses.COLOR_BLACK);s.keypad(True)
 while True:
  s.erase();s.addstr(0,0,'TUI_REAL — actual curses',curses.color_pair(1));s.addstr(1,0,'Arrow Up exits; native resize updates this screen');s.refresh();k=s.getch()
  if k==curses.KEY_RESIZE:Path('tui-resize').write_text(str(s.getmaxyx()))
  elif k==curses.KEY_UP:Path('tui-key').write_text(str(k));break
curses.wrapper(app)
''')
IDE=Path(os.environ.get('WEX_IDE',R/'target/debug/wex-ide')).resolve();CLI=Path(os.environ.get('WEX_BIN',R/'target/debug/wex')).resolve();traps=O/'traps';traps.mkdir();marker=traps/'FORBIDDEN_EXECUTED'
for n in ['gradle','gradlew','mvn','adb','emulator','bridge']:
 p=traps/n;p.write_text('#!/bin/sh\nprintf '+n+' > "'+str(marker)+'"\nexit 99\n');p.chmod(0o755)
env={**os.environ,'PATH':str(traps)+os.pathsep+os.environ['PATH'],'WEX_TOOLS_HOME':str(R/'.wex'),'WEX_PROJECT_HISTORY':str(O/'history.json'),'WEX_K2_SYSTEM_DIR':str(O/'k2-system'),'WEX_GRADLE_BRIDGE':str(traps/'bridge'),'ANDROID_HOME':'/home/zapahtravki/Android/Sdk','JAVA_HOME':'/usr/lib/jvm/java-21-openjdk'};env.pop('WAYLAND_DISPLAY',None)
x=subprocess.run([str(CLI),'resolve',str(P),'--offline'],env=env,capture_output=True,text=True,timeout=180);(O/'resolve.json').write_text(x.stdout);(O/'resolve.log').write_text(x.stderr);(O/'resolve.exit').write_text(str(x.returncode)+'\n');assert x.returncode==0,x.stderr
hashes={str(p):hashlib.sha256(p.read_bytes()).hexdigest()for p in P.rglob('*')if p.is_file()and'.wex'not in p.relative_to(P).parts}
with(O/'window.log').open('w')as log:
 proc=subprocess.Popen([str(IDE),str(P),'--terminal-smoke',str(W)],env=env,stdout=log,stderr=subprocess.STDOUT);(O/'process.json').write_text(json.dumps({'pid':proc.pid,'binary':str(IDE),'project':str(P)})+'\n')
 try:code=proc.wait(timeout=360)
 finally:
  if proc.poll()is None:
   assert str(P).encode()in Path('/proc',str(proc.pid),'cmdline').read_bytes();proc.terminate();proc.wait(timeout=15)
  # A failed probe must not leave its explicitly recorded PTY-session jobs behind.
  tracked=W/'terminal-pids.json'
  if tracked.exists():
   for sid in json.loads(tracked.read_text()):
    for entry in Path('/proc').iterdir():
     if not entry.name.isdigit():continue
     try:
      fields=(entry/'stat').read_text().rsplit(') ',1)[1].split()
      if int(fields[3])!=sid:continue
      fd=os.pidfd_open(int(entry.name));again=(entry/'stat').read_text().rsplit(') ',1)[1].split()
      if int(again[3])==sid:signal.pidfd_send_signal(fd,signal.SIGKILL)
      os.close(fd)
     except (FileNotFoundError,ProcessLookupError):pass
(O/'window.exit').write_text(str(code)+'\n');assert code==0,(O/'window.log').read_text()[-9000:]
p=json.loads((W/'terminal-window.json').read_text());assert p['result']=='GPU_PROJECT_PTY_OK'and p['renderer']=='wgpu'and len(p['screenshots'])==12
for k in ['source_restored','all_sessions_reaped','real_controlling_pty','actual_project_cwd','ansi_unicode','bracketed_before_enter_verified','ime_commit_shell_output','real_scrollback','genuine_curses_alt_screen','genuine_sigwinch_resize','independent_sessions','foreground_sigint_shell_survived','same_session_background_reaped']:assert p[k],k
assert not p['editor_save_worker_started']and p['terminal_workers']==0 and not p['deployment_invoked']and p['selected_text']=='COPY_日本'and p['curses_up_key']==259
assert all(hashlib.sha256(Path(f).read_bytes()).hexdigest()==h for f,h in hashes.items())and not marker.exists()and not(P/'.wex/native').exists()
assert(P/'terminal.cwd').read_text().strip()==str(P)and(P/'terminal.tty').read_text().startswith('/dev/pts/')and(P/'tui-key').read_text()=='259'and(P/'tui-resize').exists()
for pid in p['terminal_pids']:assert not(Path('/proc')/str(pid)).exists()
child=int((P/'terminal.child').read_text());stat=Path('/proc')/str(child)/'stat';assert not stat.exists()or stat.read_text().rsplit(') ',1)[1].startswith('Z ')
for s in p['screenshots']:
 b=Path(s['path']).read_bytes();assert b[:8]==b'\x89PNG\r\n\x1a\n'and struct.unpack('>II',b[16:24])==(s['width'],s['height'])
(O/'terminal-e2e.json').write_text(json.dumps({'result':'NATIVE_PROJECT_PTY_OK','binary':str(IDE),'binary_sha256':hashlib.sha256(IDE.read_bytes()).hexdigest(),'original_sources':hashes,'proof':p,'same_session_background_pid':child,'no_gradle_or_apk_or_phone':True},indent=2)+'\n')
print('NATIVE_PROJECT_PTY_OK: genuine controlling PTY/project cwd/ANSI Unicode/readline bracketed paste/IME/selection/scrollback/curses alternate-screen+SIGWINCH+ArrowUp/two independent shells/foreground SIGINT/shell+same-session background cleanup;source dirty guard and Undo preserved;12 genuine GPU frames;no Gradle/APK/phone',flush=True)
