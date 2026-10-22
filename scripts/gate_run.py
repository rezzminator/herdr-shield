"""Run fault controls with owned process-group and independent unit cleanup on timeout."""
import json
import os
import pathlib
import signal
import socket
import subprocess


def live(record):
    try:
        fields = pathlib.Path(f"/proc/{record['pid']}/stat").read_text().rsplit(')', 1)[1].split()
        return fields[19] == str(record['start']) and fields[0] not in ('Z', 'X')
    except FileNotFoundError:
        return False


def cleanup(root):
    records = []
    ledger = root / 'owned-resources.jsonl'
    if ledger.exists():
        records.extend(json.loads(line) for line in ledger.read_text().splitlines())
    for path in (root / 'state/herdr-shield').glob('*.json'):
        try:
            records.append(json.loads(path.read_text()))
        except json.JSONDecodeError:
            pass  # The independent ownership ledger survives deliberate registry corruption.
    units = set()
    identities = []
    for record in records:
        holder_id = record.get('id', '')
        if len(holder_id) != 32 or any(value not in '0123456789abcdef' for value in holder_id):
            continue
        unit = record.get('unit')
        assert unit == 'herdr-shield-test-' + holder_id + '.service'
        units.add(unit)
        identities.extend(record[key] for key in ('holder', 'child') if record.get(key))
    for unit in sorted(units):
        for action in ('stop', 'reset-failed'):
            subprocess.run(['systemctl', '--user', f'--machine={os.getuid()}@.host', action, unit],
                           stdout=subprocess.PIPE, stderr=subprocess.PIPE, timeout=15, check=False)
        print('CLEANUP gate unit', unit, flush=True)
        status = subprocess.run(['systemctl', '--user', f'--machine={os.getuid()}@.host', 'show',
                                 '--property=ActiveState', '--value', unit],
                                stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True, timeout=15)
        assert status.stdout.strip() not in ('active', 'activating', 'deactivating'), ('ERROR gate unit survived', unit)
    survivors = [record['pid'] for record in identities if live(record)]
    assert not survivors, ('ERROR gate process survivors', survivors)
    for path in root.rglob('*.sock'):
        assert path.is_relative_to(root) and not path.is_symlink() and path.is_socket()
        with socket.socket(socket.AF_UNIX) as connection:
            connection.settimeout(1)
            try:
                connection.connect(str(path))
            except ConnectionRefusedError:
                path.unlink()
            else:
                raise AssertionError('ERROR gate live socket: ' + str(path))
    print('CLEANUP gate process/unit/socket survivors: []', flush=True)


def run(command, *, cwd, env, timeout):
    root = pathlib.Path(env['HERDR_SHIELD_TEST_ROOT'])
    assert root.is_absolute() and root.is_relative_to(pathlib.Path(env['TMPDIR']))
    assert not root.is_symlink()
    root.mkdir(mode=0o700, parents=True, exist_ok=True)
    before = set(root.iterdir())
    process = subprocess.Popen(command, cwd=cwd, env=env, stdout=subprocess.PIPE,
                               stderr=subprocess.STDOUT, text=True, start_new_session=True)
    try:
        output, _ = process.communicate(timeout=timeout)
        return subprocess.CompletedProcess(command, process.returncode, output)
    except subprocess.TimeoutExpired:
        os.killpg(process.pid, signal.SIGKILL)
        process.communicate()
        raise
    finally:
        for path in set(root.iterdir()) - before:
            assert path.is_dir() and not path.is_symlink()
            cleanup(path)


if __name__ == '__main__':
    import sys
    environment = dict(os.environ, HERDR_SHIELD_TIMEOUT_PROBE='1')
    try:
        run([sys.argv[1], '--exact', 'coverage::gate_timeout_cleans_owned_resources', '--nocapture'],
            cwd=pathlib.Path.cwd(), env=environment, timeout=2)
    except subprocess.TimeoutExpired:
        print('PASS timed-out control: exact process group and independently recorded units cleaned', flush=True)
    else:
        raise AssertionError('timeout control did not time out')
