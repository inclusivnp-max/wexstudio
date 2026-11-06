#!/usr/bin/env python3
"""Preflight for legacy test scripts: inspect cache only, never provision Gradle."""
import os,sys
from pathlib import Path
from urllib.parse import urlparse
root=Path(sys.argv[1])
props=(root/'gradle/wrapper/gradle-wrapper.properties').read_text()
value=next(line.split('=',1)[1].strip().replace('\\:',':') for line in props.splitlines() if line.strip().startswith('distributionUrl='))
name=Path(urlparse(value).path).name.removesuffix('.zip')
version=name.removeprefix('gradle-').removesuffix('-bin').removesuffix('-all')
home=Path(os.environ.get('GRADLE_USER_HOME',str(Path.home()/'.gradle')))
found=sorted((home/'wrapper/dists'/name).glob('*/gradle-'+version+'/bin/gradle'))
if not found:
    raise SystemExit('Gradle distribution download disabled; local installation missing')
print(found[0].parent.parent)
