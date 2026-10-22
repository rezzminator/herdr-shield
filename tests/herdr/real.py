#!/usr/bin/env python3
"""Pinned real-server fault tests. Every external process belongs to this jail."""
import argparse
import fcntl
import hashlib
import json
import os
import pathlib
import pty
import select
import signal
import socket
import struct
import subprocess
import termios
import threading
import time


def identity(pid):
    try:
        stat = pathlib.Path(f'/proc/{pid}/stat').read_text().rsplit(')', 1)[1].split()
        return (pid, stat[19], stat[0])
    except FileNotFoundError:
        return None


def live(pid):
    state = identity(pid)
    return state is not None and state[2] not in ('Z', 'X')


def wait(predicate, seconds=15):
    deadline = time.monotonic() + seconds
    last = None
    while time.monotonic() < deadline:
        try:
            result = predicate()
            if result:
                return result
        except (FileNotFoundError, ConnectionRefusedError, EOFError, json.JSONDecodeError) as error:
            last = error
        time.sleep(0.05)
    raise TimeoutError(f'predicate timeout; last={last}')


class Jail:
    def __init__(self, root, herdr, keeper):
        assert root.is_absolute() and root.is_dir() and not root.is_symlink()
        assert hashlib.sha256(pathlib.Path(herdr).read_bytes()).hexdigest() == '14d9e8bc8b4a9e0118d77c32266f76d461f09b40342eb23752960bd89f59929d', 'pinned Herdr artifact required'
        self.root = root / f'h{os.getpid()}'
        assert not self.root.exists(), 'fresh jail required'
        self.root.mkdir(mode=0o700)
        self.herdr, self.keeper = herdr, keeper
        self.processes, self.units, self.clients, self.logs = [], set(), [], []
        self.handoff_pending = False
        self.env = {'HOME': str(self.root), 'PATH': '/usr/bin:/bin', 'TERM': 'xterm-256color',
                    'LANG': 'C.UTF-8', 'SHELL': '/bin/sh', 'HERDR_SHIELD_TEST_PREFIX': 'herdr-shield-test-', 'HERDR_SHIELD_TEST_BIN':keeper}
        for key, value in [('XDG_CONFIG_HOME', 'c'), ('XDG_STATE_HOME', 's'),
                           ('XDG_DATA_HOME', 'd'), ('XDG_CACHE_HOME', 'k'), ('XDG_RUNTIME_DIR', 'r')]:
            path = self.root / value
            path.mkdir(mode=0o700)
            self.env[key] = str(path)
        config = self.root / 'c/herdr'
        config.mkdir(mode=0o700)
        (config / 'config.toml').write_text(
            'onboarding = false\n[terminal]\ndefault_shell = ' + json.dumps(keeper) +
            '\nshell_mode = "non_login"\n[session]\nresume_agents_on_restore = false\n')
        self.socket = config / 'herdr.sock'
        self.env['HERDR_SOCKET_PATH'] = str(self.socket)
        keep_config = self.root / 'c/herdr-shield'
        keep_config.mkdir(mode=0o700)
        dummy = self.root / 'dummy-shell'
        fixture = pathlib.Path(__file__).resolve().parents[1] / 'fakes/dummy.sh'
        dummy.write_bytes(fixture.read_bytes())
        dummy.chmod(0o700)
        (keep_config / 'config.json').write_text(json.dumps({'shell': str(dummy), 'args': []}))
        (keep_config / 'config.json').chmod(0o600)
        plugin=self.root/'plugin'
        (plugin/'bin').mkdir(parents=True,mode=0o700)
        source_root=pathlib.Path(__file__).resolve().parents[2]
        (plugin/'herdr-plugin.toml').write_bytes((source_root/'herdr-plugin.toml').read_bytes())
        (plugin/'bin/herdr-shield').write_bytes(pathlib.Path(keeper).read_bytes())
        (plugin/'bin/herdr-shield').chmod(0o700)
        self.plugin=plugin

    def guard(self):
        assert self.socket == self.root / 'c/herdr/herdr.sock'
        assert self.env['HERDR_SOCKET_PATH'] == str(self.socket)
        assert self.socket.is_relative_to(self.root)
        assert pathlib.Path(self.env['HOME']) == self.root
        for key in ('XDG_CONFIG_HOME', 'XDG_STATE_HOME', 'XDG_DATA_HOME', 'XDG_CACHE_HOME', 'XDG_RUNTIME_DIR'):
            assert pathlib.Path(self.env[key]).is_relative_to(self.root)
        assert not self.root.is_symlink()
        print('GUARD', self.socket, flush=True)

    def command(self, *args):
        self.guard()
        return subprocess.run([self.herdr, *args], cwd=self.root, env=self.env,
                              stdout=subprocess.PIPE, stderr=subprocess.STDOUT, timeout=15, text=True)

    def api(self, method, params=None, *, endpoint=None):
        self.guard()
        endpoint=self.socket if endpoint is None else endpoint
        if endpoint!=self.socket:
            assert endpoint.name=='herdr-api.sock' and endpoint.parent.parent==self.root/'r/herdr-shield'
            assert len(endpoint.parent.name)==32 and all(ch in '0123456789abcdef' for ch in endpoint.parent.name)
        with socket.socket(socket.AF_UNIX) as connection:
            connection.settimeout(8)
            connection.connect(str(endpoint))
            connection.sendall(json.dumps({'id': 'test', 'method': method, 'params': params or {}}).encode() + b'\n')
            data = bytearray()
            while b'\n' not in data:
                chunk = connection.recv(65536)
                if not chunk:
                    raise EOFError('owned API EOF')
                data.extend(chunk)
                assert len(data) <= 1024 * 1024
        response = json.loads(data.split(b'\n', 1)[0])
        if 'error' in response:
            raise RuntimeError(response['error'])
        return response['result']

    def server(self):
        self.guard()
        log = open(self.root / f'server-{len(self.processes)}.log', 'wb')
        self.logs.append(log)
        process = subprocess.Popen([self.herdr, 'server'], env=self.env, cwd=self.root,
                                   stdout=log, stderr=subprocess.STDOUT, start_new_session=True)
        self.processes.append((process, identity(process.pid), 'server'))
        print('START server', process.pid, flush=True)
        wait(lambda: self.api('ping')['protocol'] == 22)
        return process

    def client(self):
        self.guard()
        master, slave = pty.openpty()
        fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack('HHHH', 32, 100, 0, 0))
        process = subprocess.Popen([self.herdr], env=self.env, cwd=self.root, stdin=slave,
                                   stdout=slave, stderr=slave, start_new_session=True)
        os.close(slave)
        self.processes.append((process, identity(process.pid), 'client'))
        stop = threading.Event()
        def drain():
            while not stop.is_set():
                if select.select([master], [], [], 0.1)[0]:
                    try:
                        if not os.read(master, 65536):
                            return
                    except OSError:
                        return
        thread = threading.Thread(target=drain, daemon=True)
        thread.start()
        self.clients.append((master, stop, thread))
        print('START client', process.pid, flush=True)
        return process

    def metadata(self):
        results = []
        directory = self.root / 's/herdr-shield'
        for path in directory.glob('*.json'):
            if len(path.stem) == 32:
                value = json.loads(path.read_text())
                if value.get('unit'):
                    assert value['unit'].startswith('herdr-shield-test-')
                    self.units.add(value['unit'])
                results.append(value)
        return results

    def keep(self, *args):
        self.guard()
        return subprocess.run([self.keeper, *args], env=self.env, cwd=self.root,
                              stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True, timeout=15)

    def terminate(self, process, sig=signal.SIGTERM):
        record = next(entry for entry in self.processes if entry[0] is process)
        assert record[1] is not None and identity(process.pid)[:2] == record[1][:2] if process.poll() is None else True
        if process.poll() is None:
            os.kill(process.pid, sig)
            try:
                process.wait(timeout=5)
            except subprocess.TimeoutExpired:
                os.kill(process.pid, signal.SIGKILL)
                process.wait(timeout=5)
        print('CLEANUP', record[2], process.pid, process.returncode, flush=True)

    def close(self):
        adoption_error = None
        if self.handoff_pending:
            try:
                self.record_handoff_server()
            except (AssertionError, OSError) as error:
                adoption_error = error
                print('ERROR handoff ownership could not be verified; retained unknown process', repr(error), flush=True)
        self.metadata()
        for process, _, _ in reversed(self.processes):
            self.terminate(process)
        bus = f'unix:path=/run/user/{os.getuid()}/bus'
        unit_env = dict(self.env, DBUS_SESSION_BUS_ADDRESS=bus)
        for unit in sorted(self.units):
            assert unit.startswith('herdr-shield-test-') and unit.endswith('.service')
            for action in ('stop', 'reset-failed'):
                result = subprocess.run(['systemctl', '--user', f'--machine={os.getuid()}@.host', action, unit], env=unit_env,
                                        stdout=subprocess.PIPE, stderr=subprocess.PIPE, timeout=15)
                print('CLEANUP unit', unit, action, result.returncode, flush=True)
        for master, stop, thread in self.clients:
            stop.set()
            thread.join(timeout=2)
            os.close(master)
        for log in self.logs:
            log.close()
        survivors = [process.pid for process, _, _ in self.processes if live(process.pid)]
        for meta in self.metadata():
            for key in ('holder', 'child'):
                if meta.get(key) and live(meta[key]['pid']):
                    survivors.append(meta[key]['pid'])
        print('CLEANUP survivors', survivors, flush=True)
        assert not survivors, f'ERROR: owned survivors {survivors}'
        for path in self.root.rglob('*.sock'):
            assert path.is_relative_to(self.root) and not path.is_symlink()
            if path.is_socket():
                path.unlink()
        remaining=[str(path) for path in self.root.rglob('*.sock') if path.is_socket()]
        print('CLEANUP socket survivors',remaining,flush=True)
        assert not remaining
        if adoption_error is not None:
            raise RuntimeError('handoff ownership verification failed during cleanup') from adoption_error

    def record_handoff_server(self):
        self.guard()
        with socket.socket(socket.AF_UNIX) as connection:
            connection.settimeout(5)
            connection.connect(str(self.socket))
            new_pid, uid, _ = struct.unpack('3i', connection.getsockopt(socket.SOL_SOCKET, socket.SO_PEERCRED, 12))
        assert uid == os.getuid()
        if any(process.pid == new_pid for process, _, _ in self.processes):
            return new_pid
        environment = pathlib.Path(f'/proc/{new_pid}/environ').read_bytes().split(b'\0')
        assert ('HOME=' + str(self.root)).encode() in environment
        assert pathlib.Path(f'/proc/{new_pid}/exe').resolve() == pathlib.Path(self.herdr).resolve()
        class Imported:
            pid = new_pid
            returncode = None
            def poll(self):
                if not live(self.pid):
                    self.returncode = 0
                return self.returncode
            def wait(self, timeout):
                wait(lambda: not live(self.pid), timeout)
                self.returncode = 0
                return 0
        self.processes.append((Imported(), identity(new_pid), 'handoff-server'))
        self.handoff_pending = False
        return new_pid


def exercise(jail):
    version = jail.command('--version')
    assert version.returncode == 0 and '0.9.5' in version.stdout, version.stdout
    print('ARTIFACT herdr sha256', hashlib.sha256(pathlib.Path(jail.herdr).read_bytes()).hexdigest(), flush=True)
    print('ARTIFACT keeper sha256', hashlib.sha256(pathlib.Path(jail.keeper).read_bytes()).hexdigest(), flush=True)
    server = jail.server()
    jail.api('plugin.link', {'path':str(jail.plugin)})
    plugins=jail.api('plugin.list')
    assert 'herdr-shield' in json.dumps(plugins)
    print('PASS isolated Herdr parsed pinned manifest and linked private test copy',flush=True)
    client = jail.client()
    panes = wait(lambda: jail.api('pane.list')['panes'])
    pane = panes[0]['pane_id']
    meta = wait(lambda: next((m for m in jail.metadata() if m['binding']['pane'] == pane and m['state'] == 'running'), None))
    original = identity(meta['child']['pid'])
    holder = identity(meta['holder']['pid'])
    boot=pathlib.Path('/proc/sys/kernel/random/boot_id').read_text().strip()
    assert meta['child']['boot']==boot and meta['holder']['boot']==boot
    assert original and holder
    jail.api('pane.send_text', {'pane_id': pane, 'text': 'tick\r'})
    wait(lambda: 'COUNTER=1' in jail.api('pane.read', {'pane_id': pane, 'source': 'visible'})['read']['text'])
    wait(lambda: any('session' in p.name for p in jail.root.rglob('*.json')))
    jail.terminate(server, signal.SIGKILL)
    jail.terminate(client)
    assert identity(original[0])[:2] == original[:2], 'child lost on SIGKILL'
    assert identity(holder[0])[:2] == holder[:2], 'holder lost on SIGKILL'
    server = jail.server()
    client = jail.client()
    wait(lambda: 'ambiguouspaneinstance' in ''.join(jail.api('pane.read', {'pane_id': pane, 'source': 'visible'})['read']['text'].split()))
    assert next(m for m in jail.metadata() if m['id']==meta['id'])['binding']['server']['pid']!=server.pid
    listed=jail.keep('list','--json')
    assert listed.returncode==0,listed.stderr
    coverage=json.loads(listed.stdout)
    assert next(p for p in coverage['panes'] if p['pane_id']==pane)['status']=='UNPROTECTED'
    retained=next(h for h in coverage['holders'] if h['id']==meta['id'])
    assert retained['orphan'] and 'ambiguous pane instance' in retained['reason']
    assert retained['cleanup']=='herdr-shield kill '+meta['id']
    assert identity(original[0])[:2]==original[:2]
    print('PASS ambiguous cold restore: explicit refusal, UNPROTECTED replacement, retained orphan with exact cleanup command',flush=True)
    jail.api('pane.send_text', {'pane_id':pane,'text':'attach '+meta['id']+'\r'})
    wait(lambda: 'COUNTER=1' in jail.api('pane.read', {'pane_id': pane, 'source': 'visible'})['read']['text'])
    jail.api('pane.send_text', {'pane_id': pane, 'text': 'tick\r'})
    wait(lambda: 'COUNTER=2' in jail.api('pane.read', {'pane_id': pane, 'source': 'visible'})['read']['text'])
    assert identity(original[0])[:2] == original[:2]
    assert pathlib.Path('/proc/sys/kernel/random/boot_id').read_text().strip()==boot
    assert len([m for m in jail.metadata() if m['binding']['pane'] == pane]) == 1
    print('PASS kill9 crash: same child/holder boot-start identity, counter 1 -> 2, no duplicate', flush=True)
    print('PASS reattach redraw: independently read Herdr cells contain preserved counter', flush=True)
    for _ in range(2):
        result = jail.keep('reconcile')
        assert result.returncode == 0, result.stderr
    jail.api('pane.split', {'target_pane_id': pane, 'direction': 'right'})
    other = wait(lambda: next((p['pane_id'] for p in jail.api('pane.list')['panes'] if p['pane_id'] != pane), None))
    other_meta = wait(lambda: next((m for m in jail.metadata() if m['binding']['pane'] == other and m['state'] == 'running'), None))
    time.sleep(0.3)
    jail.api('pane.close', {'pane_id': other})
    wait(lambda: not live(other_meta['child']['pid']))
    assert live(original[0]), 'close killed unrelated holder'
    print('PASS close-vs-crash: authenticated close kills matching child; crash retains original', flush=True)
    # This command intentionally bypasses the default-shell launch door.
    jail.api('layout.apply', {'workspace_id': 'w1', 'tab_label': 'unprotected', 'focus': False,
                             'root': {'type': 'pane', 'command': ['/bin/sh', '-c', 'read value']}})
    listed = jail.keep('list', '--json')
    assert listed.returncode == 0, listed.stderr
    coverage = json.loads(listed.stdout)
    assert any(p['status'] == 'UNPROTECTED' and p['reason'] for p in coverage['panes'])
    assert len(coverage['coverage']) == 14
    print('PASS explicit layout command visibly UNPROTECTED; complete 14-door coverage report', flush=True)
    moved = jail.api('pane.move', {'pane_id':pane, 'destination':{'type':'new_workspace'}, 'focus':True})
    previous_pane = pane
    pane = moved['move_result']['pane']['pane_id']
    assert pane != previous_pane
    wait(lambda: any(m['id']==meta['id'] and m['binding']['pane']==pane for m in jail.metadata()))
    jail.api('pane.send_text', {'pane_id': pane, 'text': 'tick\r'})
    wait(lambda: 'COUNTER=3' in jail.api('pane.read', {'pane_id': pane, 'source': 'visible'})['read']['text'])
    assert identity(original[0])[:2] == original[:2]
    print('PASS pane move: new public identity rebound to the original holder and child', flush=True)
    intermediate=pane
    moved=jail.api('pane.move',{'pane_id':pane,'destination':{'type':'new_workspace'},'focus':True})
    pane=moved['move_result']['pane']['pane_id']
    rebound=wait(lambda: next((m for m in jail.metadata() if m['id']==meta['id'] and m['binding']['pane']==pane),None))
    assert {previous_pane,intermediate}.issubset({binding['pane'] for binding in rebound['history']})
    relay=jail.root/'r/herdr-shield'/meta['id']/'herdr-api.sock'
    for historical in (previous_pane,intermediate):
        assert jail.api('pane.get',{'pane_id':historical},endpoint=relay)['pane']['pane_id']==pane
    assert identity(original[0])[:2]==original[:2]
    print('PASS two real pane moves: complete intermediate alias history routes both old pane IDs to current holder',flush=True)
    # Handoff is intentionally a named gap unless the combined restart is exercised below.
    before_generation=next(m for m in jail.metadata() if m['id']==meta['id'])['generation']
    jail.handoff_pending = True
    result = jail.api('server.live_handoff', {'import_exe': jail.herdr, 'expected_protocol': 22, 'expected_version': '0.9.5'})
    print('HANDOFF result', result, flush=True)
    wait(lambda: server.poll() is not None)
    new_pid = jail.record_handoff_server()
    assert new_pid != server.pid
    assert identity(original[0])[:2] == original[:2]
    rebound=False
    try:
        rebound=wait(lambda: next((m for m in jail.metadata() if m['id']==meta['id'] and m['binding']['server']['pid']==new_pid),None))
    except TimeoutError:
        pass
    assert rebound, 'holder did not rebind to the independently verified handoff server'
    jail.api('pane.send_text', {'pane_id': pane, 'text': 'tick\r'})
    wait(lambda: 'COUNTER=4' in jail.api('pane.read', {'pane_id': pane, 'source': 'visible'})['read']['text'])
    assert next(m for m in jail.metadata() if m['id']==meta['id'])['generation']==before_generation+1
    print('PASS same-binary handoff: original child retained, server generation rebound, counter input 3 -> 4 verified', flush=True)
    jail.api('pane.close', {'pane_id':pane})
    wait(lambda: not live(original[0]))
    print('PASS close after move and handoff targets the original child', flush=True)
    print('UNRUN: cross-version installer handoff, real mouse hardware, exhaustive launch-door UI exercises', flush=True)


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument('--herdr', required=True)
    parser.add_argument('--keeper', required=True)
    parser.add_argument('--root', type=pathlib.Path, required=True)
    args = parser.parse_args()
    jail = Jail(args.root, str(pathlib.Path(args.herdr).resolve()), str(pathlib.Path(args.keeper).resolve()))
    try:
        exercise(jail)
    finally:
        jail.close()


if __name__ == '__main__':
    main()
