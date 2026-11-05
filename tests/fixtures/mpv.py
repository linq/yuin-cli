#!/usr/bin/env python3
"""Deterministic mpv JSON IPC peer; never opens an audio device or network."""
import json
import os
import socket
import sys
import time

sock = socket.socket(fileno=0)
stream = sock.makefile("rwb", buffering=0)
position = float(next(arg.split("=", 1)[1] for arg in sys.argv if arg.startswith("--start=")))
volume = int(next(arg.split("=", 1)[1] for arg in sys.argv if arg.startswith("--volume=")))
paused = True
path = ""
marker = os.environ.get("YUIN_FIXTURE_MARKER")
if marker:
    with open(marker, "a", encoding="utf-8") as out:
        out.write(str(os.getpid()) + "\n")


def send(value):
    stream.write(json.dumps(value).encode() + b"\n")


def prop(name, value):
    send({"event": "property-change", "name": name, "data": value})


for line in stream:
    request = json.loads(line)
    command = request["command"]
    error = "success"
    if command[0] == "loadfile":
        path = command[1]
        if "slow-start" in path:
            time.sleep(1)
        if "invalid-audio" in path:
            send({"event": "end-file", "reason": "error"})
        else:
            send({"event": "file-loaded"})
    elif command[0] == "observe_property":
        name = command[2]
        prop(name, {"time-pos": position, "duration": 60.0, "pause": paused, "paused-for-cache": False}[name])
    elif command[0] == "set_property":
        if command[1] == "pause":
            paused = command[2]
            prop("pause", paused)
        elif command[1] == "volume":
            volume = command[2]
    elif command[0] == "seek":
        position = command[1]
        if "crash-on-seek" in path:
            sys.exit(0)
        prop("time-pos", position)
    else:
        error = "command not found"
    send({"request_id": request["request_id"], "error": error})
    if command[0] == "seek" and position >= 60:
        send({"event": "end-file", "reason": "eof"})

if marker:
    with open(marker + ".exits", "a", encoding="utf-8") as out:
        out.write(str(os.getpid()) + "\n")
