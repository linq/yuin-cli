#!/usr/bin/env python3
"""Generate a concrete Homebrew formula from built release archives."""
import argparse
import hashlib
import json
from pathlib import Path
import re

parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument('--repository', required=True, help='https://github.com/owner/repository')
parser.add_argument('--version', required=True)
parser.add_argument('--license', required=True, choices=['MIT', 'Apache-2.0', 'MIT OR Apache-2.0'])
parser.add_argument('--dist', type=Path, default=Path('dist'))
parser.add_argument('--output', type=Path, required=True)
args = parser.parse_args()
if not re.fullmatch(r'https://github.com/[A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+', args.repository):
    parser.error('Expected a GitHub repository URL')
if not re.fullmatch(r'[0-9][A-Za-z0-9.+-]*', args.version):
    parser.error('Invalid version')
if args.output.exists():
    parser.error('Output already exists')
platforms = [('macos', 'arm', 'aarch64-apple-darwin'), ('macos', 'intel', 'x86_64-apple-darwin'), ('linux', 'intel', 'x86_64-unknown-linux-gnu')]
blocks = []
for system, arch, target in platforms:
    archive = args.dist / f'yuin-{args.version}-{target}.tar.gz'
    if not archive.is_file():
        continue
    digest = hashlib.sha256(archive.read_bytes()).hexdigest()
    url = f'{args.repository}/releases/download/v{args.version}/{archive.name}'
    blocks.append(f'  on_{system} do\n    on_{arch} do\n      url {json.dumps(url)}\n      sha256 "{digest}"\n    end\n  end')
if not blocks:
    parser.error('No matching release archives found')
license_value = '{ any_of: ["MIT", "Apache-2.0"] }' if args.license == 'MIT OR Apache-2.0' else json.dumps(args.license)
formula = f'''class Yuin < Formula
  desc "Yuin CLI terminal music player"
  homepage {json.dumps(args.repository)}
  version {json.dumps(args.version)}
  license {license_value}
  on_linux do
    depends_on "alsa-lib"
  end

{chr(10).join(blocks)}

  def install
    bin.install "bin/yuin"
    man1.install "man/yuin.1"
    bash_completion.install "completions/yuin.bash" => "yuin"
    zsh_completion.install "completions/_yuin"
    fish_completion.install "completions/yuin.fish"
    doc.install "docs"
  end

  test do
    assert_match version.to_s, shell_output("#{{bin}}/yuin --version")
    assert_match "schema_version", shell_output("#{{bin}}/yuin paths --json")
  end
end
'''
args.output.parent.mkdir(parents=True, exist_ok=True)
args.output.write_text(formula)
print(args.output)
