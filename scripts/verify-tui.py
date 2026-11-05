#!/usr/bin/env python3
"""Opt-in PTY check; uses isolated data and generated silent audio.

Usage: python3 scripts/verify-tui.py target/release/yuin [--real-mpv]
Default mode uses tests/fixtures/mpv.py. --real-mpv uses mpv on PATH.
"""
import contextlib
import http.server
import threading
import fcntl
import json
import os
from pathlib import Path
import pty
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

binary = str(Path(sys.argv[1]).resolve())
real = "--real-mpv" in sys.argv[2:]
with tempfile.TemporaryDirectory(prefix="yuin-pty-") as directory:
    env = {**os.environ, "YUIN_DATA_DIR": str(Path(directory) / "data"), "TERM": "xterm-256color"}
    class CatalogFixture(http.server.BaseHTTPRequestHandler):
        searches = []
        def log_message(self, *args):
            pass
        def do_GET(self):
            parsed = urllib.parse.urlparse(self.path)
            if parsed.path == '/api/search/get':
                type(self).searches.append(urllib.parse.parse_qs(parsed.query).get('s', [''])[0])
                data = json.dumps({'code':200,'result':{'songs':[{'id':1,'name':'Search fixture','artists':[],'album':{},'duration':1000}],'songCount':1}}).encode()
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

    def read_until(needle, timeout=10):
        data = bytearray()
        deadline = time.monotonic() + timeout
        while time.monotonic() < deadline:
            ready, _, _ = select.select([master], [], [], 0.05)
            if ready:
                data.extend(os.read(master, 65536))
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
                os.read(master, 65536)

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
        # Text input owns Space/j/k/q; they must not act as playback/navigation keys.
        os.write(master, b"/j k q")
        pump()
        assert child.poll() is None
        os.write(master, b"\r")
        read_until(b"fixture")
        assert CatalogFixture.searches[-1] == 'j k q'
        os.write(master, b"a")
        deadline = time.monotonic() + 5
        while time.monotonic() < deadline:
            pump(.05)
            queue = cli("status")["queue"]
            if len(queue) == 2:
                break
        assert queue[-1]["track"]["id"] == 'wy:1', queue
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
                os.read(master, 65536)
        assert child.poll() == 0, f"Exit code: {child.poll()}"
        assert termios.tcgetattr(slave) == original, "Terminal settings were not restored"
        after = cli("status")
        assert after["state"] == "playing" and after["current"] == playing["current"]
        if real:
            time.sleep(0.3)
            assert cli("status")["position"] > after["position"]
        print(f"PTY PASS ({'real mpv' if real else 'fixture'}): controls, cross-client updates, resize, clean exit, termios restore, playback survives TUI exit")
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
