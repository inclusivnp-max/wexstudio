#!/usr/bin/env python3
"""Install the SHA-256-pinned, unmodified official Linux x64 K2 distribution."""
import argparse,hashlib,os,platform,tarfile,urllib.request
from pathlib import Path
ROOT=Path(__file__).resolve().parents[1]
VERSION='263.6379.0'
SHA256='ab8ca4455dc2fc5fe1a24db2bccc46c104254d2c465155c4251ee65df8f3f7cc'
if platform.system()!='Linux' or platform.machine() not in ('x86_64','AMD64'):
    raise SystemExit('Set WEX_KOTLIN_LSP to your platform-specific JetBrains Kotlin LSP launcher')
args=argparse.ArgumentParser();args.add_argument('--offline',action='store_true');options=args.parse_args()
tools=Path(os.environ.get('WEX_TOOLS_HOME',ROOT/'.wex')).resolve()/'tools';tools.mkdir(parents=True,exist_ok=True)
archive=tools/f'kotlin-server-{VERSION}.tar.gz'
if not archive.exists():
    if options.offline:raise SystemExit('Pinned Kotlin LSP archive missing in configured tool data; offline preparation performed no downloads')
    temporary=archive.with_suffix('.download')
    urllib.request.urlretrieve(f'https://download.jetbrains.com/language-server/kotlin-server/{VERSION}/{archive.name}',temporary)
    temporary.replace(archive)
hash=hashlib.sha256()
with archive.open('rb') as f:
    for block in iter(lambda:f.read(1024*1024),b''):hash.update(block)
if hash.hexdigest()!=SHA256:raise SystemExit('Kotlin LSP archive SHA-256 mismatch')
destination=tools/'kotlin-lsp'
server=destination/f'kotlin-server-{VERSION}'/'bin/intellij-server'
if not server.exists():
    with tarfile.open(archive) as t:t.extractall(destination,filter='data')
assert os.access(server,os.X_OK)
print(f'K2_READY: {server}\nSHA256: {SHA256}')
