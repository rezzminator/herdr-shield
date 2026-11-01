#!/usr/bin/python3
"""Owned raw PTY oracle; results are bytes read by the child, never shield status."""
import json
import os
import pathlib
import select
import sys
import termios
import time
import tty

root = pathlib.Path(os.environ['HOME'])
tty.setraw(0)
mode = sys.argv[1]
if mode == 'queries':
    os.write(1, b'QUERY_READY')
    while not (root / 'query-go').exists():
        time.sleep(0.005)
    corpus = b'\x1b[c\x1b[5n\x1b[6n\x1b]10;?\x07\x1b[18t'
    for byte in corpus:
        os.write(1, bytes([byte]))
    # Fill output before reading stdin: the holder must drain independently of saturated input.
    for _ in range(1024):
        os.write(1, b'y' * 1024)
    # Hold the reader closed while the saturated holder processes the query burst.
    time.sleep(0.2)
    data = bytearray()
    deadline = time.monotonic() + 4
    while time.monotonic() < deadline:
        if select.select([0], [], [], 0.1)[0]:
            data.extend(os.read(0, 65536))
    # Rename publishes the reply file whole: the reader waits only for it to exist.
    (root / 'query-replies.tmp').write_bytes(data)
    (root / 'query-replies.tmp').rename(root / 'query-replies')
elif mode == 'flood':
    os.write(1, b'FLOOD_READY')
    while not (root / 'flood-go').exists():
        time.sleep(0.005)
    for _ in range(32768):
        os.write(1, b'x' * 1022 + b'\r\n')
    os.write(1, b'\x1b[2J\x1b[H\x1b[?2004h\x1b[?1006h\x1b[?1002h\x1b[?1004h\x1b[4;7HFINAL\x1b[?25l')
    (root / 'flood-done').write_text('33554432')
elif mode == 'input':
    # A clipboard write made before any attach must never reach a later attachment.
    os.write(1, b'\x1b]52;c;REVUQUNIRUQ=\x07')
    os.write(1, b'\x1b[2J\x1b[H\x1b[?1049h\x1b[?2004h\x1b[?1006h\x1b[?1002h\x1b[?1004h\x1b[?1h\x1b=\x1b[2;3H\xe7\x95\x8ce\xcc\x81\x1b]8;;https://example.invalid/\x1b\\link\x1b]8;;\x1b\\')
    data = bytearray()
    while True:
        data.extend(os.read(0, 65536))
        (root / 'input-transcript').write_bytes(data)
        if data.endswith(b'DONE'):
            break
    os.write(1, b'\x1bcRESET_OK\x1b]52;c;TElWRQ==\x07')
while True:
    time.sleep(1)
