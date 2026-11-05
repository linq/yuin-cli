#!/usr/bin/env python3
"""Create a portable archive, checksums, and (on macOS) an unsigned installer.
Does not install software, upload releases, or modify shell profiles.
"""
import argparse
import hashlib
import io
import json
import os
from pathlib import Path
import re
import shutil
import subprocess
import tarfile
import tempfile

root = Path(__file__).resolve().parent.parent
parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument('--target', required=True, choices=['aarch64-apple-darwin', 'x86_64-apple-darwin', 'x86_64-unknown-linux-gnu', 'aarch64-unknown-linux-gnu'])
parser.add_argument('--binary', type=Path)
parser.add_argument('--output', type=Path, default=root / 'dist')
parser.add_argument('--pkg', action='store_true', help='also create a macOS installer with pkgbuild')
args = parser.parse_args()
version = re.search(r'^version\s*=\s*"([^"]+)"', (root / 'Cargo.toml').read_text(), re.M)[1]
binary = (args.binary or root / 'target' / args.target / 'release/yuin').resolve()
if not binary.is_file():
    parser.error(f'Build the release binary first: {binary}')
args.output.mkdir(parents=True, exist_ok=True)
name = f'yuin-{version}-{args.target}'
archive = args.output / f'{name}.tar.gz'
if archive.exists():
    parser.error(f'Archive already exists: {archive}')
with tempfile.TemporaryDirectory(prefix='yuin-package-') as temp:
    staging = Path(temp) / name
    (staging / 'bin').mkdir(parents=True)
    shutil.copyfile(binary, staging / 'bin/yuin')
    (staging / 'bin/yuin').chmod(0o755)
    for path in ['README.md', 'README.zh-CN.md', 'docs/THIRD_PARTY_NOTICES.md', 'docs/TECHNICAL_DESIGN.zh-CN.md', 'docs/STATUS.md', 'packaging/INSTALL.md']:
        target = staging / path
        target.parent.mkdir(parents=True, exist_ok=True)
        shutil.copyfile(root / path, target)
    shutil.copytree(root / 'docs/licenses', staging / 'docs/licenses', dirs_exist_ok=True, copy_function=shutil.copyfile)
    for license_name in ['LICENSE', 'LICENSE-MIT', 'LICENSE-APACHE']:
        if (root / license_name).is_file():
            shutil.copyfile(root / license_name, staging / license_name)
    dependencies = subprocess.run([str(root / 'scripts/cargo.sh'), 'metadata', '--format-version', '1', '--locked'], cwd=root, check=True, capture_output=True, text=True)
    packages = json.loads(dependencies.stdout)['packages']
    license_report = [{'name': p['name'], 'version': p['version'], 'license': p.get('license'), 'repository': p.get('repository')} for p in packages]
    (staging / 'docs/dependency-licenses.json').write_text(json.dumps(license_report, indent=2) + '\n')
    licenses = staging / 'docs/licenses/dependencies'
    for package in packages:
        package_root = Path(package['manifest_path']).parent
        if package_root == root:
            continue
        destination = licenses / f"{package['name']}-{package['version']}"
        copied = False
        for candidate in package_root.iterdir():
            if candidate.is_file() and candidate.name.upper().startswith(('LICENSE', 'LICENCE', 'COPYING', 'NOTICE', 'COPYRIGHT')):
                destination.mkdir(parents=True, exist_ok=True)
                shutil.copyfile(candidate, destination / candidate.name)
                copied = True
        if not copied:
            destination.mkdir(parents=True, exist_ok=True)
            (destination / 'LICENSE-REFERENCE.txt').write_text(f"SPDX: {package.get('license')}\nUpstream: {package.get('repository')}\n")
    # Generate on a native runner. Cross-builds can supply completions in a later native packaging step.
    for shell in ['bash', 'zsh', 'fish']:
        result = subprocess.run([str(binary), 'completions', shell], capture_output=True)
        if result.returncode:
            parser.error(f'The package binary must be runnable on this runner: {result.stderr.decode(errors="replace")}')
        path = staging / 'completions' / {'bash': 'yuin.bash', 'zsh': '_yuin', 'fish': 'yuin.fish'}[shell]
        path.parent.mkdir(exist_ok=True)
        path.write_bytes(result.stdout)
    man = subprocess.run([str(binary), 'man'], capture_output=True, check=True)
    (staging / 'man').mkdir()
    (staging / 'man/yuin.1').write_bytes(man.stdout)
    for entry in [staging, *staging.rglob('*')]:
        if hasattr(os, 'listxattr'):
            for attribute in os.listxattr(entry):
                os.removexattr(entry, attribute)
    with tarfile.open(archive, 'w:gz') as output:
        output.add(staging, arcname=name)
    artifacts = [archive]
    if args.pkg:
        if 'apple-darwin' not in args.target or not shutil.which('pkgbuild'):
            parser.error('--pkg requires macOS and an Apple target')
        payload = Path(temp) / 'payload'
        (payload / 'usr/local/bin').mkdir(parents=True)
        shutil.copyfile(binary, payload / 'usr/local/bin/yuin')
        (payload / 'usr/local/bin/yuin').chmod(0o755)
        for kind, source, target in [('manual', staging / 'man/yuin.1', payload / 'usr/local/share/man/man1/yuin.1')]:
            target.parent.mkdir(parents=True, exist_ok=True)
            shutil.copyfile(source, target)
        doc = payload / 'usr/local/share/doc/yuin'
        shutil.copytree(staging / 'docs', doc, copy_function=shutil.copyfile)
        shutil.copyfile(root / 'packaging/INSTALL.md', doc / 'INSTALL.md')
        for license_name in ['LICENSE', 'LICENSE-MIT', 'LICENSE-APACHE']:
            if (staging / license_name).is_file():
                shutil.copyfile(staging / license_name, doc / license_name)
        installer = args.output / f'{name}.pkg'
        if installer.exists():
            parser.error(f'Installer already exists: {installer}')
        # macOS installer component versions must be numeric.
        numeric_version = re.match(r'\d+(?:\.\d+)*', version)[0]
        subprocess.run(['pkgbuild', '--root', str(payload), '--identifier', 'dev.linq.yuin-cli', '--version', numeric_version, '--install-location', '/', str(installer)], check=True, env={**os.environ, 'COPYFILE_DISABLE': '1'})
        artifacts.append(installer)
    records = []
    for artifact in artifacts:
        digest = hashlib.sha256(artifact.read_bytes()).hexdigest()
        artifact.with_name(artifact.name + '.sha256').write_text(f'{digest}  {artifact.name}\n')
        records.append({'name': artifact.name, 'sha256': digest, 'bytes': artifact.stat().st_size})
    (args.output / f'{name}.manifest.json').write_text(json.dumps({'schema_version': 1, 'version': version, 'target': args.target, 'artifacts': records, 'mpv_bundled': False}, indent=2) + '\n')
    print(json.dumps(records, indent=2))
