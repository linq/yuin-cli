#!/usr/bin/env python3
"""Explicit real-service acceptance using a user-selected trusted LX source.
Third-party scripts/audio remain temporary. Playback uses volume zero.
"""
import argparse
import contextlib
import json
import os
from pathlib import Path
import re
import shutil
import subprocess
import tempfile
import time

parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument('binary', type=Path)
parser.add_argument('--source', required=True, help='Trusted local source snapshot or HTTP(S) URL')
parser.add_argument('--platforms', nargs='+', choices=['wy', 'kw', 'tx', 'kg', 'mg'], default=['wy', 'kw', 'tx', 'kg', 'mg'])
parser.add_argument('--query', default='稻香')
parser.add_argument('--quality', choices=['128k', '320k', 'flac', 'flac24bit'], default='128k')
parser.add_argument('--download-platform', default='kw')
parser.add_argument('--report', type=Path)
args = parser.parse_args()
binary = str(args.binary.resolve())
location = args.source if args.source.startswith(('http://', 'https://')) else str(Path(args.source).resolve())
report = {'schema_version': 1, 'platforms': {}, 'offline': None}

def safe(value):
    return re.sub(r'https?://[^\s"<>]+', '[URL]', str(value))

with tempfile.TemporaryDirectory(prefix='yuin-real-') as root:
    env = {**os.environ, 'YUIN_DATA_DIR': root + '/data'}
    env.pop('YUIN_CATALOG_BASE', None)
    def cli(*command):
        result = subprocess.run([binary, *map(str, command), '--json'], env=env, capture_output=True, text=True, timeout=125)
        if result.returncode:
            raise RuntimeError(safe(json.loads(result.stderr).get('error', {}).get('message', result.stderr)))
        return json.loads(result.stdout)
    runtime = cli('paths')['runtime']
    try:
        cli('volume', 0)
        cli('config', 'set', 'play_quality', args.quality)
        source = cli('source', 'import', location)['source']
        report['source'] = {key: source[key] for key in ['name', 'version', 'sha256']}
        cli('source', 'use', source['id'])
        download_track = None
        for platform in args.platforms:
            checks = {}
            try:
                search = cli('search', args.query, '--platform', platform)
                checks['search'] = {'count': len(search['items']), 'total': search['total']}
                assert search['items'], 'No tracks returned'
                track = search['items'][0]
                checks['track_id'] = track['id']
                for kind in ['playlists', 'charts']:
                    try:
                        collection = cli(kind, 'list', '--platform', platform)
                        checks[kind] = {'count': len(collection['items']), 'total': collection.get('total'), 'has_more': collection['has_more']}
                        if collection['items']:
                            page = cli(kind, 'show', collection['items'][0]['id'])
                            checks[kind]['tracks'] = len(page['items'])
                            checks[kind]['track_total'] = page['total']
                    except Exception as error:
                        checks[kind] = {'error': safe(error)}
                cli('play', track['id'])
                deadline = time.monotonic() + 15
                while time.monotonic() < deadline:
                    status = cli('status')
                    if status['state'] == 'error':
                        raise RuntimeError(status['error'])
                    if status['state'] == 'playing' and status['position'] > .2:
                        break
                    time.sleep(.1)
                else:
                    raise RuntimeError('mpv did not advance in 15 seconds')
                checks['playback'] = 'passed'
                cli('stop')
                if platform == args.download_platform:
                    download_track = track
            except Exception as error:
                checks['error'] = safe(error)
            report['platforms'][platform] = checks
            print(platform, json.dumps(checks, ensure_ascii=False), flush=True)
        if download_track:
            task = cli('download', download_track['id'], '--quality', args.quality)['download']
            deadline = time.monotonic() + 300
            while time.monotonic() < deadline:
                row = next(item for item in cli('downloads', 'list')['items'] if item['id'] == task['id'])
                if row['state'] in ['completed', 'failed', 'cancelled']:
                    break
                time.sleep(.5)
            assert row['state'] == 'completed', row.get('error', row['state'])
            cli('source', 'stop')
            cli('play', download_track['id'])
            time.sleep(.5)
            status = cli('status')
            assert status['state'] == 'playing' and status['position'] > .2
            report['offline'] = {'track_id': download_track['id'], 'report': row['report'], 'source_disabled': True, 'playback': 'passed'}
            print('offline', json.dumps(report['offline'], ensure_ascii=False), flush=True)
    finally:
        with contextlib.suppress(Exception):
            cli('daemon', 'stop')
        time.sleep(.2)
        shutil.rmtree(runtime, ignore_errors=True)
if args.report:
    args.report.parent.mkdir(parents=True, exist_ok=True)
    args.report.write_text(json.dumps(report, ensure_ascii=False, indent=2) + '\n')
if not report['offline']:
    raise SystemExit('No complete download/offline flow passed; inspect the per-platform report')
