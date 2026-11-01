#!/usr/bin/env python3
"""Broken-jail and failed-adoption controls clean only independently owned processes."""
import argparse
import json
import os
import pathlib
import subprocess
import sys
import signal
import socket
import struct
from unittest.mock import patch

sys.path.insert(0, str(pathlib.Path(__file__).resolve().parent))
from real import PINS, Jail, identity, live, wait


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument('--herdr', required=True)
    parser.add_argument('--keeper', required=True)
    parser.add_argument('--root', type=pathlib.Path, required=True)
    args = parser.parse_args()
    jail = Jail(args.root, str(pathlib.Path(args.herdr).resolve()), str(pathlib.Path(args.keeper).resolve()))
    outside_env = dict(jail.env, HOME=str(args.root))
    outside = subprocess.Popen(['/bin/sleep', '60'], env=outside_env)
    owned = None
    closed = False
    try:
        outside_id = identity(outside.pid)
        jail.processes.append((outside, outside_id, 'outside-control'))
        try:
            jail.terminate(outside)
        except AssertionError as error:
            assert 'outside owned jail' in str(error)
        else:
            raise AssertionError('broken jail signalled an outside-HOME process')
        assert live(outside.pid) and identity(outside.pid)[:2] == outside_id[:2]
        with patch.object(pathlib.Path, 'read_bytes', side_effect=PermissionError('INJECTED_LIVE_INACCESSIBLE')):
            try:
                jail.terminate(outside)
            except PermissionError:
                pass
            else:
                raise AssertionError('inaccessible live process was treated as exited')
        assert live(outside.pid) and identity(outside.pid)[:2] == outside_id[:2]
        jail.processes.pop()
        print('PASS broken-jail outside-process control: no signal sent', flush=True)
        exiting = subprocess.Popen(['/bin/sleep', '1'], env=jail.env)
        jail.processes.append((exiting, identity(exiting.pid), 'exit-race-control'))
        original_read = pathlib.Path.read_bytes
        def exit_during_environment_read(path):
            if path == pathlib.Path(f'/proc/{exiting.pid}/environ'):
                # Emulate the kernel's inaccessible exiting-mm window after the live poll.
                exiting.wait(timeout=3)
                raise PermissionError('INJECTED_EXITING_MM')
            return original_read(path)
        with patch.object(pathlib.Path, 'read_bytes', exit_during_environment_read):
            jail.terminate(exiting)
        assert exiting.poll() == 0
        print('PASS exact owned-client exit race: inaccessible environment accepted only after observed exit', flush=True)
        owned = jail.server()
        # Failed handoff adoption is a hard error after recorded resources are cleaned.
        jail.handoff_pending = True
        jail.api('server.live_handoff', {'import_exe': jail.herdr, 'expected_protocol': 22, 'expected_version': PINS['HERDR_VERSION']})
        wait(lambda: owned.poll() is not None)
        if receipt := os.environ.get('HERDR_SHIELD_HANDOFF_FAILURE_RECEIPT'):
            jail.guard()
            with socket.socket(socket.AF_UNIX) as connection:
                connection.connect(str(jail.socket))
                pid, uid, _ = struct.unpack('3i', connection.getsockopt(socket.SOL_SOCKET, socket.SO_PEERCRED, 12))
            assert uid == os.getuid()
            pathlib.Path(receipt).write_text(json.dumps(dict(pid=pid, start=identity(pid)[1], root=str(jail.root))))
            raise RuntimeError('INJECTED_EARLY_HANDOFF_FAILURE')
        original_adoption = jail.record_handoff_server
        adoption_failed = False
        def failed_adoption():
            nonlocal adoption_failed
            if not adoption_failed:
                adoption_failed = True
                raise OSError('INJECTED_HANDOFF_ADOPTION_FAILURE')
            return original_adoption()
        jail.record_handoff_server = failed_adoption
        jail.handoff_pending = True
        original_terminate = jail.terminate
        failed_once = False
        def failed_cleanup(process):
            nonlocal failed_once
            if not failed_once:
                failed_once = True
                raise OSError('INJECTED_CLEANUP_FAILURE')
            return original_terminate(process)
        jail.terminate = failed_cleanup
        try:
            jail.close()
        except RuntimeError as error:
            assert 'handoff ownership verification failed' in str(error)
            closed = True
        else:
            raise AssertionError('injected adoption failure was hidden')
        assert not live(owned.pid)
        assert failed_once
        assert any(role == 'handoff-server' for _, _, role in jail.processes)
        assert all(not live(process.pid) for process, _, _ in jail.processes)
        print('PASS actual live handoff with injected cleanup/adoption failures: visible errors, independently verified adoption recovery, zero owned survivors', flush=True)
    finally:
        # This is the exact independently started negative-control process, never an enumerated target.
        assert identity(outside.pid)[:2] == outside_id[:2] if outside.poll() is None else True
        outside.terminate()
        outside.wait(timeout=5)
        print('CLEANUP outside negative-control', outside.pid, flush=True)
        if not closed:
            jail.close()


if __name__ == '__main__':
    if 'HERDR_SHIELD_HANDOFF_FAILURE_RECEIPT' not in os.environ:
        build_root = pathlib.Path(sys.argv[sys.argv.index('--root') + 1])
        receipt = build_root / ('handoff-failure-' + str(os.getpid()) + '.json')
        result = subprocess.run([sys.executable, '-I', '-B', __file__, *sys.argv[1:]],
                                env=dict(os.environ, HERDR_SHIELD_HANDOFF_FAILURE_RECEIPT=str(receipt)),
                                stdout=subprocess.PIPE, stderr=subprocess.STDOUT, text=True, timeout=90)
        print(result.stdout, end='', flush=True)
        record = json.loads(receipt.read_text())
        leaked = live(record['pid']) and identity(record['pid'])[1] == record['start']
        if leaked:
            # Clean the broken control by its independently recorded exact identity.
            environment = pathlib.Path(f"/proc/{record['pid']}/environ").read_bytes().split(b'\0')
            assert ('HOME=' + record['root']).encode() in environment
            os.kill(record['pid'], signal.SIGTERM)
            wait(lambda: not live(record['pid']))
        receipt.unlink()
        assert result.returncode != 0 and 'INJECTED_EARLY_HANDOFF_FAILURE' in result.stdout
        assert not leaked, 'early handoff assertion leaked imported server'
        print('PASS injected early handoff assertion: imported server adopted and cleaned with zero survivors', flush=True)
    main()
