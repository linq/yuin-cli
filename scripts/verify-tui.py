#!/usr/bin/env python3
"""Opt-in PTY check; uses isolated data and generated silent audio.

Usage: python3 scripts/verify-tui.py target/release/yuin [--real-mpv] [--large-playlist]
Default mode uses tests/fixtures/mpv.py. --real-mpv uses mpv on PATH.
--large-playlist checks 649 tracks and snapshots larger than 1 MiB.
"""
import contextlib
import http.server
import threading
import fcntl
import json
import os
from pathlib import Path
import pty
import re
import select
import shutil
import signal
import struct
import subprocess
import sys
import tempfile
import termios
import time
import wave
import urllib.parse
import zlib

binary = str(Path(sys.argv[1]).resolve())
real = "--real-mpv" in sys.argv[2:]
playlist_size = 649 if '--large-playlist' in sys.argv[2:] else 2
with tempfile.TemporaryDirectory(prefix="yuin-pty-") as directory:
    env = {**os.environ, "YUIN_DATA_DIR": str(Path(directory) / "data"), "TERM": "xterm-256color", "TERM_PROGRAM": "WezTerm", "COLORTERM": "truecolor"}
    env.pop('NO_COLOR', None)
    def png_chunk(kind, data):
        return struct.pack('!I', len(data)) + kind + data + struct.pack('!I', zlib.crc32(kind + data))
    def cover_image(color):
        return b'\x89PNG\r\n\x1a\n' + png_chunk(b'IHDR', struct.pack('!IIBBBBB', 2, 2, 8, 6, 0, 0, 0)) + png_chunk(b'IDAT', zlib.compress((b'\x00' + bytes(color) * 2) * 2)) + png_chunk(b'IEND', b'')
    covers = {'/playlist-cover.png': cover_image((255,0,0,255)), '/cover.png': cover_image((0,0,255,255)), '/cover2.png': cover_image((0,255,0,255)), '/cover3.png': cover_image((255,255,0,255))}
    class CatalogFixture(http.server.BaseHTTPRequestHandler):
        searches = []
        cover_requests = []
        def log_message(self, *args):
            pass
        def do_GET(self):
            parsed = urllib.parse.urlparse(self.path)
            if parsed.path == '/api/search/get':
                query = urllib.parse.parse_qs(parsed.query)
                kind = query.get('type', [''])[0]
                type(self).searches.append((query.get('s', [''])[0], kind))
                if kind == '1000':
                    data = json.dumps({'code':200,'result':{'playlists':[{'id':10,'name':'Playlist fixture','coverImgUrl':f'http://127.0.0.1:{self.server.server_port}/playlist-cover.png'}],'playlistCount':1}}).encode()
                else:
                    data = json.dumps({'code':200,'result':{'songs':[{'id':n,'name':f'Search fixture {n}','artists':[],'album':{'picUrl':f'http://127.0.0.1:{self.server.server_port}/cover{n if n>1 else ""}.png'},'duration':60000} for n in range(1,4)],'songCount':3}}).encode()
            elif parsed.path in covers:
                type(self).cover_requests.append(parsed.path)
                data = covers[parsed.path]
            elif parsed.path == '/audio.wav':
                data = audio.read_bytes()
            elif parsed.path == '/api/v3/playlist/detail':
                data = json.dumps({'code':200,'playlist':{'trackCount':playlist_size,'trackIds':[{'id':n} for n in range(1,playlist_size+1)]}}).encode()
            elif parsed.path == '/api/v3/song/detail':
                ids = json.loads(urllib.parse.parse_qs(parsed.query)['ids'][0])
                data = json.dumps({'code':200,'songs':[{'id':n,'name':f'Playlist track {n}','ar':[],'al':{'id':8,'name':'海'*700 if playlist_size>2 else 'Album','picUrl':f'http://127.0.0.1:{self.server.server_port}/cover.png'},'dt':60000} for n in ids]}).encode()
            else:
                data = b'{"code":200,"playlists":[],"list":[],"total":0}'
            self.send_response(200)
            self.send_header('Content-Length', str(len(data)))
            self.end_headers()
            with contextlib.suppress(BrokenPipeError, ConnectionResetError):
                self.wfile.write(data)
    catalog = http.server.ThreadingHTTPServer(('127.0.0.1', 0), CatalogFixture)
    threading.Thread(target=catalog.serve_forever, daemon=True).start()
    env['YUIN_CATALOG_BASE'] = f'http://127.0.0.1:{catalog.server_port}'
    env["YUIN_MPV"] = shutil.which("mpv") if real else str(Path(__file__).resolve().parent.parent / "tests/fixtures/mpv.py")
    if not env["YUIN_MPV"]:
        raise RuntimeError("mpv is required for --real-mpv")

    def cli(*args):
        result = subprocess.run([binary, *args, "--json"], env=env, capture_output=True, text=True, timeout=25, check=True)
        return json.loads(result.stdout)

    runtime = Path(cli("paths")["runtime"])
    audio = Path(directory) / "静音 测试.wav"
    with wave.open(str(audio), "wb") as output:
        output.setparams((1, 2, 8000, 0, "NONE", "not compressed"))
        output.writeframes(b"\0" * 8000 * 2 * 60)
    master, slave = pty.openpty()
    original = termios.tcgetattr(slave)
    fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack("HHHH", 30, 120, 0, 0))
    child = None
    transcript = bytearray()

    def read_output():
        data = os.read(master, 65536)
        transcript.extend(data)
        return data

    def image_commands(offset=0):
        return [tuple(map(int, values)) for values in re.findall(rb'\x1b_Ga=T,[^;]*?i=(\d+),[^;]*?c=(\d+),r=(\d+)', transcript[offset:])]

    def read_until(needle, timeout=10):
        data = bytearray()
        deadline = time.monotonic() + timeout
        while time.monotonic() < deadline:
            ready, _, _ = select.select([master], [], [], 0.05)
            if ready:
                data.extend(read_output())
                if needle in data:
                    return
            if child.poll() is not None:
                break
        raise RuntimeError(f"Missing terminal output: {needle!r}; exit={child.poll()}; output={bytes(data[-1000:])!r}")

    def pump(duration=0.25):
        deadline = time.monotonic() + duration
        while time.monotonic() < deadline:
            ready, _, _ = select.select([master], [], [], 0.025)
            if ready:
                read_output()

    def wait_state(expected):
        deadline = time.monotonic() + 5
        while time.monotonic() < deadline:
            pump(0.05)
            snapshot = cli("status")
            if snapshot["state"] == expected:
                pump()
                return snapshot
            time.sleep(0.025)
        raise RuntimeError(f"Wanted {expected}; got {snapshot}")

    try:
        cli("config", "set", "language", "en")
        source = Path(directory) / 'source.js'
        source.write_text("/* @name PTY fixture */\nlx.on(lx.EVENT_NAMES.request,async()=>({url:'BASE/audio.wav'}));lx.send(lx.EVENT_NAMES.inited,{sources:{wy:{type:'music',actions:['musicUrl'],qualitys:['128k','320k']}}});".replace('BASE', f'http://127.0.0.1:{catalog.server_port}'))
        source_id = cli('source', 'import', str(source))['source']['id']
        cli('source', 'use', source_id)
        playing = cli("play", str(audio))
        child = subprocess.Popen([binary, "tui"], stdin=slave, stdout=slave, stderr=slave, env=env)
        read_until(b"leave")
        pump()
        os.write(master, b" ")
        wait_state("paused")
        os.write(master, b" ")
        wait_state("playing")
        # Verify changes made by an independent CLI process reach the live TUI.
        cli("volume", "37")
        read_until(b"37")
        # Search playlists in the playlist page, then switch to song search.
        os.write(master, b"/playlist\r")
        read_until(b"Playlist fixture")
        assert CatalogFixture.searches[-1] == ('playlist', '1000')
        pump(.4)
        assert '/playlist-cover.png' in CatalogFixture.cover_requests
        os.write(master, b'F')
        deadline = time.monotonic() + 5
        while time.monotonic() < deadline:
            pump(.05)
            lists = cli('library', 'list')['items']
            if lists:
                break
        assert len(lists) == 1 and lists[0]['count'] == playlist_size, lists
        pump()
        os.write(master, b'F')
        pump(.3)
        assert len(cli('library', 'list')['items']) == 1
        os.write(master, b'b')
        deadline = time.monotonic() + 10
        expected_tracks = [f'wy:{n}' for n in range(1,playlist_size+1)]
        while time.monotonic() < deadline:
            pump(.05)
            whole = cli('status')
            if [e['track']['id'] for e in whole['queue']] == expected_tracks and whole['state'] == 'playing':
                break
        assert [e['track']['id'] for e in whole['queue']] == expected_tracks, whole
        assert whole['current'] == whole['queue'][0]['id'] and whole['state'] == 'playing'
        cli('queue', 'clear')
        playing = cli('play', str(audio))
        pump()
        os.write(master, b"1")
        read_until(b"Search")
        # Text input owns Space/j/k/q; they must not act as playback/navigation keys.
        os.write(master, b"/j k q")
        pump()
        assert child.poll() is None
        os.write(master, b"\r")
        read_until(b"fixture")
        assert CatalogFixture.searches[-1] == ('j k q', '1')
        os.write(master, b"a")
        deadline = time.monotonic() + 5
        while time.monotonic() < deadline:
            pump(.05)
            queue = cli("status")["queue"]
            if len(queue) == 2:
                break
        assert queue[-1]["track"]["id"] == 'wy:1', queue
        if not real:
            marker = len(transcript)
            song_playing = cli('play', 'wy:1')
            read_until(b'\x1b_Ga=T,f=100')
            pump(.4)
            thumbnail = [image for image in image_commands(marker) if image[1:] == (8,4)]
            assert thumbnail, image_commands(marker)
            playing_image_id = thumbnail[-1][0]
            preview_image_id = next(image[0] for image in image_commands() if image[1] >= 12 and image[2] >= 5)
            assert playing_image_id != preview_image_id
            marker = len(transcript)
            os.write(master, b'jjk')
            pump(1)
            assert CatalogFixture.cover_requests.count('/cover2.png') == 1, CatalogFixture.cover_requests
            assert '/cover3.png' not in CatalogFixture.cover_requests, CatalogFixture.cover_requests
            assert cli('status')['current'] == song_playing['current']
            assert any(image[0] == preview_image_id for image in image_commands(marker))
            assert all(image[0] != playing_image_id for image in image_commands(marker))
            marker = len(transcript)
            cli('queue', 'add', 'wy:3')
            song_playing = cli('next')
            read_until(b'\x1b_Ga=T,f=100')
            pump(.4)
            assert CatalogFixture.cover_requests.count('/cover3.png') == 1
            assert any(image[0] == playing_image_id for image in image_commands(marker))
            assert all(image[0] != preview_image_id for image in image_commands(marker))
            requests = len(CatalogFixture.cover_requests)
            os.write(master, b'k')
            pump(.4)
            assert len(CatalogFixture.cover_requests) == requests, 'Song cover should be cached'
            marker = len(transcript)
            os.write(master, b'2')
            pump(.4)
            assert len(CatalogFixture.cover_requests) == requests, 'Playlist cover should be cached'
            assert cli('status')['current'] == song_playing['current']
            assert any(image[0] == preview_image_id for image in image_commands(marker))
            assert all(image[0] != playing_image_id for image in image_commands(marker))
            marker = len(transcript)
            playing = cli('play', str(audio))
            read_until(b'\x1b_Ga=d,d=I')
            deleted = re.findall(rb'\x1b_Ga=d,d=I,i=(\d+),q=2',transcript[marker:])
            assert str(playing_image_id).encode() in deleted
            assert str(preview_image_id).encode() not in deleted
        os.write(master, b"3")
        read_until(b"Charts")
        fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack("HHHH", 12, 60, 0, 0))
        child.send_signal(signal.SIGWINCH)
        read_until(b"\x1b")
        os.write(master, b"q")
        deadline = time.monotonic() + 5
        while child.poll() is None and time.monotonic() < deadline:
            ready, _, _ = select.select([master], [], [], 0.05)
            if ready:
                read_output()
        assert child.poll() == 0, f"Exit code: {child.poll()}"
        assert termios.tcgetattr(slave) == original, "Terminal settings were not restored"
        after = cli("status")
        assert after["state"] == "playing" and after["current"] == playing["current"]
        if real:
            time.sleep(0.3)
            assert cli("status")["position"] > after["position"]
        print(f"PTY PASS ({'real mpv' if real else 'fixture'}): {playlist_size}-track playlist F/b, independent cover previews/cache/debounce, controls, resize and clean exit")
    finally:
        if child and child.poll() is None:
            child.kill()
            child.wait(timeout=5)
        os.close(master)
        os.close(slave)
        subprocess.run([binary, "daemon", "stop"], env=env, capture_output=True, timeout=25)
        deadline = time.monotonic() + 3
        while runtime.joinpath("daemon.sock").exists() and time.monotonic() < deadline:
            time.sleep(0.02)
        shutil.rmtree(runtime, ignore_errors=True)
        catalog.shutdown()
        catalog.server_close()
