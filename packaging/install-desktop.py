#!/usr/bin/env python3
"""Opt-in menu shortcut. Never changes associations/autostart or replaces a file."""
import argparse,os
from pathlib import Path
p=argparse.ArgumentParser();p.add_argument('--applications',type=Path,default=Path(os.environ.get('XDG_DATA_HOME',Path.home()/'.local/share'))/'applications');a=p.parse_args();app=Path(__file__).resolve().parent
assert not any(c in str(app) for c in '\n\r\0"`$\\'),'Package directory contains unsupported desktop-entry quoting characters'
template=(app/'share/applications/wexstudio.desktop.in').read_text();content=template.replace('@APP_DIR@',str(app));a.applications.mkdir(parents=True,exist_ok=True);target=a.applications/'wexstudio.desktop'
with target.open('x') as f:f.write(content)
print('DESKTOP_ENTRY_CREATED: '+str(target))
