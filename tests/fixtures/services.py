#!/usr/bin/env python3
"""Local-only S2/S3 acceptance: real QuickJS, HTTP fixtures, SQLite and CLI."""
import contextlib
import concurrent.futures
import http.server
import io
import json
import os
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile
import threading
import time
import urllib.parse
import wave

binary = os.environ['YUIN_TEST_BINARY']
wave_bytes = io.BytesIO()
with wave.open(wave_bytes, 'wb') as output:
    output.setparams((1, 2, 8000, 0, 'NONE', 'not compressed'))
    output.writeframes(b'\0' * 8000 * 2 * 4)
audio = wave_bytes.getvalue()


def song(number):
    return {'id': number, 'name': f'Track {number}', 'ar': [{'name': 'Artist'}], 'al': {'id': 8, 'name': Server.album_name}, 'dt': 4000, 'h': {'br': 320000}}


class Server(http.server.BaseHTTPRequestHandler):
    protocol_version = 'HTTP/1.1'
    source = ''
    truncated = False
    track_count = 101
    album_name = 'Album'
    audio_mode = 'valid'
    active = 0
    maximum = 0
    lock = threading.Lock()

    def handle(self):
        with contextlib.suppress(ConnectionResetError, BrokenPipeError):
            super().handle()

    def log_message(self, *args):
        pass

    def do_GET(self):
        parsed = urllib.parse.urlparse(self.path)
        query = urllib.parse.parse_qs(parsed.query)
        path = parsed.path
        if path == '/source.js':
            return self.respond(self.source.encode(), 'text/javascript')
        if path == '/echo':
            return self.respond(json.dumps({'ok': True}).encode())
        if path == '/audio.wav':
            if self.headers.get('X-Yuin-Test') != 'yes':
                return self.respond(b'forbidden', code=403)
            if self.audio_mode == 'html':
                return self.respond(b'<html>not audio</html>', 'text/html')
            if self.audio_mode == 'slow':
                with self.lock:
                    type(self).active += 1
                    type(self).maximum = max(self.maximum, self.active)
                try:
                    self.send_response(200)
                    self.send_header('Content-Length', str(len(audio) * 20))
                    self.end_headers()
                    for _ in range(20):
                        self.wfile.write(audio)
                        self.wfile.flush()
                        time.sleep(.05)
                except (BrokenPipeError, ConnectionResetError):
                    pass
                finally:
                    with self.lock:
                        type(self).active -= 1
                return
            return self.respond(audio, 'audio/wav')
        if path == '/api/song/lyric':
            return self.respond(json.dumps({'code': 200, 'lrc': {'lyric': '[00:00.00]first\n[00:01.00]second'}, 'tlyric': {'lyric': '[00:01.00]translation'}}).encode())
        if path == '/api/search/get':
            if query.get('type') == ['1000']:
                return self.respond(json.dumps({'code': 200, 'result': {'playlists': [{'id': 10, 'name': 'Fixture playlist', 'creator': {'nickname': 'Fixture'}}], 'playlistCount': 31}}).encode())
            return self.respond(json.dumps({'code': 200, 'result': {'songs': [song(1), song(2)], 'songCount': 2}}).encode())
        if path == '/r.s':
            return self.respond(b"{'TOTAL':'1','abslist':[{'playlistid':'10','name':'Fixture playlist','nickname':'Fixture'}]}")
        if path == '/api/v3/search/special':
            return self.respond(json.dumps({'errcode': 0, 'data': {'info': [{'specialid': 10, 'specialname': 'Fixture playlist', 'nickname': 'Fixture'}], 'total': 1}}).encode())
        if path == '/soso/fcgi-bin/client_music_search_songlist':
            return self.respond(json.dumps({'code': 0, 'data': {'list': [{'dissid': '10', 'dissname': 'Fixture playlist', 'creator': {'name': 'Fixture'}}], 'sum': 1}}).encode())
        if path == '/music_search/v3/search/searchAll':
            return self.respond(json.dumps({'songListResultData': {'result': [{'id': '10', 'name': 'Fixture playlist', 'userName': 'Fixture'}], 'totalCount': 1}}).encode())
        if path == '/api/playlist/list':
            return self.respond(json.dumps({'code': 200, 'total': 1, 'playlists': [{'id': 10, 'name': 'All 101 tracks', 'creator': {'nickname': 'Fixture'}}]}).encode())
        if path == '/api/toplist':
            return self.respond(json.dumps({'code': 200, 'list': [{'id': 10, 'name': 'Chart'}]}).encode())
        if path == '/api/v3/playlist/detail':
            ids = [{'id': i} for i in range(1, self.track_count + 1)]
            if self.truncated:
                ids.pop()
            return self.respond(json.dumps({'code': 200, 'playlist': {'trackCount': self.track_count, 'trackIds': ids, 'tracks': [song(1)] if ids else []}}).encode())
        if path == '/api/v3/song/detail':
            ids = json.loads(query['ids'][0])
            # Deliberately unordered detail response; adapter must restore ordered IDs.
            return self.respond(json.dumps({'code': 200, 'songs': [song(i) for i in reversed(ids)]}).encode())
        return self.respond(b'unknown fixture path', code=404)

    def respond(self, data, content_type='application/json', code=200):
        self.send_response(code)
        self.send_header('Content-Type', content_type)
        self.send_header('Content-Length', str(len(data)))
        self.end_headers()
        self.wfile.write(data)


server = http.server.ThreadingHTTPServer(('127.0.0.1', 0), Server)
server.daemon_threads = True
threading.Thread(target=server.serve_forever, daemon=True).start()
base = f'http://127.0.0.1:{server.server_port}'
source_code = '''/*
 * @name Fixture LX source
 * @version 1.0
 */
const {EVENT_NAMES,utils}=lx;
if(typeof process!=='undefined'||typeof require!=='undefined')throw Error('native environment exposed');
if(utils.crypto.md5('hello')!=='5d41402abc4b2a76b9719d911017c592')throw Error('md5');
const key=utils.buffer.from('000102030405060708090a0b0c0d0e0f','hex');
const plain=utils.buffer.from('00112233445566778899aabbccddeeff','hex');
if(utils.crypto.aesEncrypt(plain,'aes-128-ecb',key,[]).toString('hex').slice(0,32)!=='69c4e0d86a7b0430d8cdb78070b4c55a')throw Error('aes');
lx.on(EVENT_NAMES.request,async ({info})=>{
 if(info.musicInfo.albumId!=='8'||info.musicInfo.songmid!==Number(info.musicInfo.songmid))throw Error('lost music metadata');
 const inflated=await utils.zlib.inflate(await utils.zlib.deflate(utils.buffer.from('hello')));
 if(inflated.toString()!=='hello')throw Error('zlib');
 await new Promise((resolve,reject)=>lx.request('BASE/echo',{},(error,response,body)=>error?reject(error):(body.ok?resolve():reject(Error('body')))));
 return {url:'BASE/audio.wav',headers:{'X-Yuin-Test':'yes'}};
});
setTimeout(()=>lx.send(EVENT_NAMES.inited,{sources:{wy:{type:'music',actions:['musicUrl'],qualitys:['128k','320k','flac','flac24bit']}}}),5);
'''.replace('BASE', base)
Server.source = source_code

with tempfile.TemporaryDirectory(prefix='yuin-services-') as root:
    env = {**os.environ, 'YUIN_DATA_DIR': root + '/data', 'YUIN_AUDIO_OUTPUT': 'null', 'YUIN_CATALOG_BASE': base}
    def output(*args):
        begin = time.monotonic()
        result = subprocess.run([binary, *map(str, args), '--json'], env=env, capture_output=True, text=True, timeout=100)
        if os.environ.get("YUIN_TEST_TIMINGS") and time.monotonic() - begin > .5:
            print("TIMING", args[:2], round(time.monotonic() - begin, 2), flush=True)
        return result
    def cli(*args):
        result = output(*args)
        assert result.returncode == 0, (args, result.stderr)
        assert not result.stderr, result.stderr
        return json.loads(result.stdout)
    def fail(code, *args):
        result = output(*args)
        assert result.returncode == code, (args, result.returncode, result.stderr)
        assert not result.stdout
        return json.loads(result.stderr)
    def download_state(task, state, timeout=10):
        deadline = time.monotonic() + timeout
        while time.monotonic() < deadline:
            record = next(r for r in cli('downloads', 'list')['items'] if r['id'] == task)
            if record['state'] == state:
                return record
            if record['state'] in ('failed', 'completed', 'cancelled'):
                assert record['state'] == state, record
            time.sleep(.03)
        raise AssertionError(('download timeout', record))
    runtime = cli('paths')['runtime']
    try:
        search = cli('search', 'fixture', '--platform', 'wy')
        assert len(search['items']) == 2 and search['total'] == 2
        for platform in ('wy', 'kw', 'kg', 'tx', 'mg'):
            playlists = cli('playlists', 'search', 'fixture', '--platform', platform)
            assert playlists['items'][0]['id'] == f'{platform}:10', playlists
            assert playlists['items'][0]['title'] == 'Fixture playlist'
        assert cli('playlists', 'search', 'fixture', '--platform', 'wy')['has_more']
        fail(4, 'play', 'wy:1')
        assert cli('status')['state'] == 'error'
        imported = cli('source', 'import', base + '/source.js')['source']
        assert imported['name'] == 'Fixture LX source' and imported['version'] == '1.0'
        cli('source', 'use', imported['id'])
        assert cli('play', 'wy:1')['state'] == 'playing'
        # Failed candidate updates preserve the prior hash and active source.
        Server.source = 'throw Error("broken update");'
        fail(4, 'source', 'update', imported['id'])
        assert cli('source', 'list')['items'][0]['sha256'] == imported['sha256']
        Server.source = source_code
        cli('source', 'update', imported['id'])
        first = cli('playlists', 'show', 'wy:10')
        second = cli('playlists', 'show', 'wy:10', '--page', '2')
        assert len(first['items']) == 100 and first['has_more'] and len(second['items']) == 1
        assert [t['song_id'] for t in first['items']] == list(map(str, range(1, 101)))
        saved = cli('library', 'save', 'wy:10', '完整歌单')['playlist']
        assert len(saved['tracks']) == 101
        with concurrent.futures.ThreadPoolExecutor(max_workers=2) as pool:
            favorites = list(pool.map(lambda _: cli('playlists', 'favorite', 'wy:10', '--name', '收藏整单'), range(2)))
        favorite = favorites[0]['playlist']
        assert favorites[1]['playlist']['id'] == favorite['id']
        assert favorite['count'] == 101
        repeated = cli('playlists', 'favorite', 'wy:10', '--name', 'do not rename')
        assert repeated['already_saved'] and repeated['playlist']['id'] == favorite['id']
        assert cli('library', 'show', favorite['id'])['playlist']['name'] == '收藏整单'
        assert len(cli('library', 'list')['items']) == 2
        whole = cli('playlists', 'play', 'wy:10')
        assert whole['state'] == 'playing' and len(whole['queue']) == 101
        assert [e['track']['song_id'] for e in whole['queue']] == list(map(str, range(1, 102)))
        assert whole['current'] == whole['queue'][0]['id']
        assert cli('next')['current'] == whole['queue'][1]['id']
        local = cli('library', 'play', favorite['id'])
        assert local['state'] == 'playing' and len(local['queue']) == 101
        assert local['current'] == local['queue'][0]['id']
        cli('mode', 'shuffle')
        shuffled = cli('library', 'play', favorite['id'])
        assert shuffled['mode'] == 'shuffle' and len(shuffled['queue']) == 101
        assert cli('next')['current'] != shuffled['current']
        cli('mode', 'sequential')
        # A realistic larger snapshot must cross the old 1 MiB transport cap.
        Server.track_count = 649
        Server.album_name = 'Album ' + '海' * 700
        large = cli('playlists', 'play', 'wy:10')
        assert large['state'] == 'playing' and len(large['queue']) == 649
        assert [e['track']['song_id'] for e in large['queue']] == list(map(str, range(1, 650)))
        assert len(json.dumps(large, ensure_ascii=False).encode()) > 1024 * 1024
        assert len(cli('status')['queue']) == 649
        assert cli('next')['current'] == large['queue'][1]['id']
        cli('daemon', 'stop')
        deadline = time.monotonic() + 5
        while Path(runtime, 'daemon.sock').exists() and time.monotonic() < deadline:
            time.sleep(.02)
        assert not Path(runtime, 'daemon.sock').exists()
        restored_large = cli('status')
        assert restored_large['state'] == 'paused' and restored_large['queue'] == large['queue']
        assert restored_large['current'] == large['queue'][1]['id']
        Server.track_count = 101
        Server.album_name = 'Album'
        cli('playlists', 'play', 'wy:10')
        before = cli('status')
        for count in (0,):
            Server.track_count = count
            fail(2, 'playlists', 'play', 'wy:10')
            retained = cli('status')
            assert retained['queue'] == before['queue'] and retained['current'] == before['current']
        Server.track_count = 101
        empty = cli('library', 'create', 'empty')['playlist']
        fail(2, 'library', 'play', empty['id'])
        assert cli('status')['queue'] == before['queue']
        cli('library', 'delete', empty['id'])
        cli('library', 'delete', favorite['id'])
        Server.truncated = True
        fail(3, 'library', 'save', 'wy:10', 'must not save a preview')
        fail(3, 'playlists', 'favorite', 'wy:11')
        fail(3, 'playlists', 'play', 'wy:10')
        assert cli('status')['queue'] == before['queue']
        Server.truncated = False
        assert len(cli('library', 'list')['items']) == 1
        cli('library', 'move', saved['id'], '0', '100')
        assert cli('library', 'show', saved['id'], '--page', '2')['playlist']['tracks'][-1]['id'] == 'wy:1'
        cli('config', 'set', 'play_quality', '128k')
        fail(2, 'config', 'set', 'download_concurrency', '3')
        task = cli('download', 'wy:1', '--quality', '128k')['download']
        completed = download_state(task['id'], 'completed')
        target = Path(completed['path'])
        assert target.exists() and completed['report']['codec'] == 'pcm'
        duplicate = cli('download', 'wy:1', '--quality', '128k')['download']
        download_state(duplicate['id'], 'failed')
        assert target.read_bytes() == audio
        for quality in ('320k', 'flac', 'flac24bit'):
            task = cli('download', 'wy:2', '--quality', quality)['download']
            download_state(task['id'], 'failed')
        Server.audio_mode = 'html'
        task = cli('download', 'wy:2', '--quality', '128k')['download']
        download_state(task['id'], 'failed')
        Server.audio_mode = 'slow'
        tasks = [cli('download', f'wy:{n}', '--quality', '128k')['download']['id'] for n in range(3, 7)]
        deadline = time.monotonic() + 5
        while Server.active < 2 and time.monotonic() < deadline:
            time.sleep(.02)
        assert Server.maximum <= 2, Server.maximum
        for task in reversed(tasks):
            cli('downloads', 'cancel', task)
        for task in tasks:
            download_state(task, 'cancelled')
        assert not list(Path(root + '/data/downloads').glob('*.part'))
        # Recovery removes only this daemon's recorded partial file, never a shared-directory stranger.
        stranger = Path(root + '/data/downloads/.yuin-unrelated.part')
        stranger.write_bytes(b'not our task')
        cli('daemon', 'stop')
        time.sleep(.2)
        daemon = subprocess.Popen([binary, 'daemon', 'run'], env=env, stdin=subprocess.DEVNULL, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
        deadline = time.monotonic() + 5
        while not Path(runtime, 'daemon.sock').exists():
            assert daemon.poll() is None and time.monotonic() < deadline
            time.sleep(.01)
        try:
            interrupted = cli('download', 'wy:9', '--quality', '128k')['download']['id']
            running = download_state(interrupted, 'downloading')
            part = Path(running['partial_path'])
            assert part.exists()
            daemon.kill()
            daemon.wait(timeout=5)
        finally:
            if daemon.poll() is None:
                daemon.kill()
                daemon.wait(timeout=5)
        cli('status')
        assert cli('downloads', 'show', interrupted)['download']['state'] == 'cancelled'
        assert not part.exists() and stranger.read_bytes() == b'not our task'
        lyrics = cli('lyrics', 'wy:1')['lyrics']
        assert len(lyrics['lines']) == 2 and lyrics['lines'][1]['translation'] == 'translation'
        export_path = Path(root) / 'backup.json'
        cli('export', export_path, '--include-sources')
        assert export_path.stat().st_mode & 0o777 == 0o600
        preview = cli('import', export_path)
        assert not preview['write_performed'] and preview['preview']['playlists'] == 1
        assert len(cli('library', 'list')['items']) == 1
        imported_bundle = cli('import', export_path, '--commit')
        assert Path(imported_bundle['backup']).is_file()
        assert len(cli('library', 'list')['items']) == 2
        assert cli('source', 'list')['active'] == imported['id']
        migrated = next(source for source in cli('source', 'list')['items'] if source['id'] != imported['id'])
        assert not migrated['capabilities']
        cli('source', 'use', migrated['id'])
        assert cli('source', 'list')['items'][0]['capabilities']
        desktop_path = Path(root) / 'desktop.json'
        desktop_path.write_text(json.dumps({'schema': 1, 'playlists': [{'name': 'Desktop', 'tracks': [{'platform': 'wy', 'songID': '3', 'title': 'Desktop track', 'artist': 'Artist', 'album': 'Album', 'duration': 4, 'sourceInfo': {'albumId': '8'}}]}], 'sources': [], 'downloads': [{'track': search['items'][0], 'quality': '128k', 'status': 'completed', 'file': '/missing/user-selected-export.wav'}], 'queue': {'entries': [1]}}))
        desktop_preview = cli('import', desktop_path)['preview']
        assert desktop_preview['unavailable_downloads'] == 1 and desktop_preview['queue_entries_not_imported'] == 1
        cli('import', desktop_path, '--commit')
        assert len(cli('library', 'list')['items']) == 3
        cli('source', 'stop')
        # No enabled source and no HTTP server: the same track must use verified local audio.
        server.shutdown()
        assert cli('play', 'wy:1')['state'] == 'playing'
        cli('pause')
        cli('daemon', 'stop')
        time.sleep(.2)
        restored = cli('status')
        assert restored['state'] == 'paused'
        assert cli('config', 'get')['config']['play_quality'] == '128k'
        assert cli('library', 'show', saved['id'])['total'] == 101
        assert len(cli('library', 'show', saved['id'], '--page', '2')['items']) == 1
        assert cli('resume')['state'] == 'playing'
        assert cli('lyrics', 'wy:1')['lyrics'] == lyrics
        print('PASS services: real QuickJS crypto/timers/HTTP, source rollback, complete paging, library/config persistence, validated downloads, cancel, no overwrite, offline restart')
    finally:
        with contextlib.suppress(Exception):
            output('daemon', 'stop')
        time.sleep(.2)
        shutil.rmtree(runtime, ignore_errors=True)
        server.server_close()
