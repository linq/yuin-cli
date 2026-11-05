#!/usr/bin/env python3
"""Local audio/catalog server for real decoder cancellation and failure tests."""
import contextlib
import http.server
import json
from pathlib import Path
import sys
import time

root = Path(sys.argv[1])
class Server(http.server.BaseHTTPRequestHandler):
    def log_message(self, *args):
        pass
    def do_GET(self):
        if self.path.startswith('/api/v3/song/detail'):
            data = json.dumps({'code': 200, 'songs': [{'id': 1, 'name': 'Slow audio', 'ar': [], 'al': {}, 'dt': 60000}]}).encode()
        else:
            data = (root / 'slow-start.wav').read_bytes()
            (root / 'starts').write_text('HTTP audio request\n')
            # Delay actual media, not the decoder implementation.
            time.sleep(1)
        self.send_response(200)
        self.send_header('Content-Length', str(len(data)))
        self.end_headers()
        with contextlib.suppress(BrokenPipeError, ConnectionResetError):
            self.wfile.write(data)
server = http.server.ThreadingHTTPServer(('127.0.0.1', 0), Server)
server.daemon_threads = True
print(f'http://127.0.0.1:{server.server_port}', flush=True)
server.serve_forever()
