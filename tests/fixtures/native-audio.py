#!/usr/bin/env python3
"""Native decoder acceptance: real HTTP buffering, headers, seeks and cancellation."""
import contextlib
import base64
import http.server
import io
import json
import os
from pathlib import Path
import shutil
import subprocess
import tempfile
import threading
import time
import wave

binary = os.environ['YUIN_TEST_BINARY']
raw = io.BytesIO()
with wave.open(raw, 'wb') as output:
    output.setparams((2, 2, 16000, 0, 'NONE', 'not compressed'))
    output.writeframes(b'\x00\x10\x00\xf0' * 16000 * 12)
audio = raw.getvalue()
class Server(http.server.BaseHTTPRequestHandler):
    mode = 'stream'
    payload = audio
    requested = threading.Event()
    complete = threading.Event()
    release = threading.Event()
    headers_seen = []
    def log_message(self, *args):
        pass
    def do_GET(self):
        if self.path.startswith('/api/v3/song/detail'):
            data = json.dumps({'code': 200, 'songs': [{'id': 1, 'name': 'Native audio', 'ar': [], 'al': {}, 'dt': 12000}]}).encode()
            self.send_response(200); self.send_header('Content-Length', str(len(data))); self.end_headers(); self.wfile.write(data)
            return
        type(self).headers_seen.append(self.headers.get('X-Yuin-Test'))
        type(self).requested.set()
        mode = type(self).mode
        payload = type(self).payload
        prefix = 64044 if payload is audio else max(len(payload)*2//3, min(len(payload)-1, 1536))
        self.send_response(200)
        if mode != 'unknown':
            self.send_header('Content-Length', str(512 * 1024 * 1024 + 1 if mode == 'oversize' else len(payload)))
        self.end_headers()
        with contextlib.suppress(BrokenPipeError, ConnectionResetError):
            if mode == 'oversize':
                return
            if mode == 'html':
                self.wfile.write(b'<html>not audio</html>')
                return
            if mode == 'stall':
                type(self).release.wait(10)
                return
            self.wfile.write(payload[:prefix]); self.wfile.flush()
            if mode == 'truncated':
                return
            type(self).release.wait(10)
            self.wfile.write(payload[prefix:]); self.wfile.flush()
            type(self).complete.set()
server = http.server.ThreadingHTTPServer(('127.0.0.1', 0), Server)
server.daemon_threads = True
threading.Thread(target=server.serve_forever, daemon=True).start()
base = f'http://127.0.0.1:{server.server_port}'
with tempfile.TemporaryDirectory(prefix='yuin-native-') as root:
    env = {**os.environ, 'YUIN_DATA_DIR': root+'/data', 'YUIN_CATALOG_BASE': base, 'YUIN_AUDIO_OUTPUT': 'null', 'YUIN_MPV': '/missing-mpv-must-not-be-used'}
    def run(*args, ok=True):
        output = subprocess.run([binary, *args, '--json'], env=env, capture_output=True, text=True, timeout=15)
        assert (output.returncode == 0) == ok, (args, output.returncode, output.stderr)
        return json.loads(output.stdout if ok else output.stderr)
    def wait_state(state):
        deadline = time.monotonic()+6
        while time.monotonic() < deadline:
            snapshot = run('status')
            if snapshot['state'] == state:
                return snapshot
            time.sleep(.025)
        raise AssertionError((state, snapshot))
    runtime = Path(run('paths')['runtime'])
    try:
        source = Path(root)/'source.js'
        source.write_text("/* @name Native audio */\nlx.on(lx.EVENT_NAMES.request,async()=>({url:'BASE/audio.wav',headers:{'X-Yuin-Test':'yes'}}));lx.send(lx.EVENT_NAMES.inited,{sources:{wy:{type:'music',actions:['musicUrl'],qualitys:['128k','320k']}}});".replace('BASE',base))
        identifier = run('source','import',str(source))['source']['id']
        run('source','use',identifier)
        for mode in ['stream', 'unknown']:
            Server.mode = mode; Server.release.clear(); Server.complete.clear(); Server.requested.clear()
            playing = run('play','wy:1')
            assert playing['state'] == 'playing'
            assert not Server.complete.is_set(), 'Playback waited for the whole HTTP file'
            time.sleep(.15)
            run('pause')
            before = run('status')['position']
            time.sleep(.2)
            assert abs(run('status')['position']-before) < .08
            # Keep the seek waiting across multiple 100 ms monitor ticks.
            # Queued pre-seek positions must not overwrite its acknowledged target.
            threading.Timer(.35, Server.release.set).start()
            sought = run('seek','5')
            assert abs(sought['position']-5) < .05, sought
            for _ in range(8):
                snapshot = run('status')
                assert abs(snapshot['position']-5) < .05, snapshot
                time.sleep(.025)
            run('volume','23'); run('resume')
            deadline = time.monotonic()+2
            while run('status')['position'] <= 5:
                assert time.monotonic() < deadline, run('status')
                time.sleep(.025)
            run('seek','12'); wait_state('ended')
        formats = json.loads(Path(__file__).with_name('audio-formats.json').read_text())
        for kind in ['mp3', 'm4a']:
            Server.payload = base64.b64decode(formats[kind])
            Server.mode = 'stream'; Server.release.clear(); Server.complete.clear()
            assert run('play', 'wy:1')['state'] == 'playing'
            assert not Server.complete.is_set(), f'{kind} waited for the whole file'
            Server.release.set()
            run('pause'); run('seek', '1'); run('resume')
            time.sleep(.1)
            assert run('status')['position'] >= 1
            run('stop')
        Server.payload = audio
        for mode in ['truncated','html','oversize']:
            Server.mode = mode; Server.release.set()
            output = subprocess.run([binary,'play','wy:1','--json'],env=env,capture_output=True,text=True,timeout=10)
            if output.returncode:
                assert json.loads(output.stderr)['error']['code'] == 5
            wait_state('error')
            run('stop')
        Server.mode = 'stall'; Server.release.clear(); Server.requested.clear()
        child = subprocess.Popen([binary,'play','wy:1','--json'],env=env,stdout=subprocess.PIPE,stderr=subprocess.PIPE,text=True)
        assert Server.requested.wait(3)
        start = time.monotonic()
        run('stop')
        assert time.monotonic()-start < .5, 'Stop blocked on network loading'
        child.communicate(timeout=3)
        assert run('status')['state'] == 'idle'
        assert Server.headers_seen and all(header == 'yes' for header in Server.headers_seen)
        print('PASS native audio: progressive/unknown-length HTTP, headers, pause/seek/EOF, truncation/HTML/size failures and stalled-load cancellation')
    finally:
        Server.release.set()
        subprocess.run([binary,'daemon','stop'],env=env,capture_output=True,timeout=10)
        deadline = time.monotonic()+3
        while runtime.joinpath('daemon.sock').exists() and time.monotonic() < deadline:
            time.sleep(.02)
        shutil.rmtree(runtime,ignore_errors=True)
server.shutdown(); server.server_close()
