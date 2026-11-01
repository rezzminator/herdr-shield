#!/usr/bin/env python3
"""A local test process, never a real agent or external service."""
import json
import os
import pathlib
import socket
import subprocess
import sys


def send(request):
    endpoint = pathlib.Path(os.environ['HERDR_SOCKET_PATH'])
    assert endpoint.is_relative_to(pathlib.Path(os.environ['XDG_RUNTIME_DIR']))
    assert endpoint.name == 'herdr-api.sock'
    with socket.socket(socket.AF_UNIX) as connection:
        connection.settimeout(10)
        connection.connect(str(endpoint))
        connection.sendall(json.dumps(request).encode() + b'\n')
        data = bytearray()
        while b'\n' not in data:
            chunk = connection.recv(65536)
            if not chunk:
                raise EOFError('test relay EOF')
            data.extend(chunk)
        return json.loads(data.split(b'\n', 1)[0])


if len(sys.argv) > 1 and sys.argv[1] == 'resumed':
    pathlib.Path(os.environ['HOME'], 'resumed.json').write_text(json.dumps(sys.argv))
elif len(sys.argv) > 1 and sys.argv[1] == '--send':
    print(json.dumps({'pid': os.getpid(), 'response': send(json.loads(sys.argv[2]))}))
else:
    print('DUMMY_AGENT_READY', flush=True)
    for line in sys.stdin:
        command = json.loads(line)
        request = command['request']
        if command.get('helper'):
            result = subprocess.run([sys.executable, '-I', __file__, '--send', json.dumps(request)],
                                    check=True, stdout=subprocess.PIPE, text=True)
            result = json.loads(result.stdout)
        else:
            result = {'pid': os.getpid(), 'response': send(request)}
        with pathlib.Path(os.environ['HOME'], 'responses.jsonl').open('a') as log:
            log.write(json.dumps(result) + '\n')
        print('REPORT_COMPLETE', request['id'], flush=True)
