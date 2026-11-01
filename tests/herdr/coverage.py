#!/usr/bin/env python3
"""Independent pinned-server launch-door, stream, refusal and identity oracles."""
import argparse
import json
import os
import pathlib
import select
import signal
import socket
import sys
import time

sys.path.insert(0, str(pathlib.Path(__file__).resolve().parent))
from real import Jail, identity, live, wait


class Stream:
    def __init__(self, jail, endpoint):
        jail.guard()
        assert endpoint.is_relative_to(jail.root) and not endpoint.is_symlink()
        self.socket = socket.socket(socket.AF_UNIX)
        self.socket.settimeout(8)
        self.socket.connect(str(endpoint))
        self.file = self.socket.makefile('rb')

    def send(self, method, params=None, request_id='stream'):
        self.socket.sendall(json.dumps(dict(id=request_id, method=method, params=params or {})).encode() + b'\n')

    def read(self):
        value = self.file.readline(1024 * 1024 + 1)
        assert value and len(value) <= 1024 * 1024, 'ERROR: missing/bounded typed frame'
        return json.loads(value)

    def close(self):
        self.socket.shutdown(socket.SHUT_RDWR)
        self.file.close()
        self.socket.close()


def report(jail, action=False):
    result = jail.keep('action-list' if action else 'list', *([] if action else ['--json']))
    assert result.returncode == 0, (result.stdout, result.stderr)
    value = json.loads(result.stdout)
    assert value['enumeration'] == 'OK', value
    return value


def census(jail):
    config = jail.root / 'c/herdr/config.toml'
    original = config.read_text()
    # A pane created before configuration cannot be adopted retroactively.
    config.write_text(original.replace(jail.keeper, str(jail.root / 'dummy-shell')))
    server = jail.server()
    jail.client()
    preexisting = wait(lambda: jail.api('pane.list')['panes'])[0]['pane_id']
    wait(lambda: b'PLAIN_READY' in jail.client_output[-1])
    for action in (False, True):
        assert next(p for p in report(jail, action)['panes'] if p['pane_id'] == preexisting)['status'] == 'UNPROTECTED'
    config.write_text(original + '\n[keys]\nprefix = "ctrl+b"\nnew_workspace = "prefix+w"\nnew_tab = "prefix+c"\nsplit_vertical = "prefix+v"\nsplit_horizontal = "prefix+s"\n')
    jail.api('server.reload_config')
    master = jail.clients[-1][0]
    protected = []

    def shell_created(operation, label):
        before = {p['pane_id'] for p in jail.api('pane.list')['panes']}
        operation()
        try:
            pane = wait(lambda: next((p['pane_id'] for p in jail.api('pane.list')['panes'] if p['pane_id'] not in before), None))
        except TimeoutError as error:
            raise AssertionError((label, bytes(jail.client_output[-1][-2000:]))) from error
        meta = wait(lambda: next((m for m in jail.metadata() if m['binding']['pane'] == pane and m['state'] == 'running'), None))
        wait(lambda: next(p for p in report(jail)['panes'] if p['pane_id'] == pane)['status'] == 'PROTECTED')
        assert identity(meta['child']['pid'])[:2] == (meta['child']['pid'], str(meta['child']['start']))
        protected.append((pane, meta))
        print('PASS census default-shell', label, pane, meta['child']['pid'], flush=True)
        return pane

    def key(value):
        os.write(master, b'\x02')
        time.sleep(0.05)
        os.write(master, value)
        time.sleep(0.05)
        os.write(master, b'\r')

    pane = shell_created(lambda: key(b'c'), 'UI new tab')
    shell_created(lambda: key(b'v'), 'UI ordinary vertical split')
    shell_created(lambda: key(b's'), 'UI ordinary horizontal split')
    shell_created(lambda: key(b'w'), 'UI new workspace')
    shell_created(lambda: jail.api('workspace.create', {'label': 'api-workspace'}), 'API workspace.create')
    shell_created(lambda: jail.api('tab.create', {'workspace_id': 'w1'}), 'API tab.create')
    shell_created(lambda: jail.api('pane.split', {'target_pane_id': pane, 'direction': 'right'}), 'API pane.split')
    shell_created(lambda: jail.api('layout.apply', {'workspace_id': 'w1', 'root': {'type': 'pane'}}), 'layout default-shell new tab')
    before_layout = {p['pane_id'] for p in jail.api('pane.list')['panes']}
    shell_created(lambda: jail.api('layout.apply', {'workspace_id': 'w1', 'root': {'type': 'split', 'direction': 'right', 'ratio': 0.5, 'first': {'type': 'pane'}, 'second': {'type': 'pane'}}}), 'layout default-shell split')
    layout_children = [p['pane_id'] for p in jail.api('pane.list')['panes'] if p['pane_id'] not in before_layout]
    assert len(layout_children) == 2
    for layout_child in layout_children:
        wait(lambda: next(p for p in report(jail)['panes'] if p['pane_id'] == layout_child)['status'] == 'PROTECTED')
    run_pane, run_meta = protected[-1]
    result = jail.command('pane', 'run', run_pane, 'tick')
    assert result.returncode == 0, result.stdout
    wait(lambda: 'COUNTER=1' in jail.api('pane.read', {'pane_id': run_pane, 'source': 'visible'})['read']['text'])
    assert next(m for m in jail.metadata() if m['id'] == run_meta['id'])['child'] == run_meta['child']
    assert next(p for p in report(jail)['panes'] if p['pane_id'] == run_pane)['status'] == 'PROTECTED'
    result = jail.command('pane', 'run', preexisting, 'tick')
    assert result.returncode == 0, result.stdout
    wait(lambda: 'COUNTER=1' in jail.api('pane.read', {'pane_id': preexisting, 'source': 'visible'})['read']['text'])
    assert next(p for p in report(jail)['panes'] if p['pane_id'] == preexisting)['status'] == 'UNPROTECTED'
    # Every bypass gets a separate dummy PID oracle and a visible reason on both surfaces.
    door = jail.root / 'door-shell'
    # Written then renamed, so a reader never sees the created-but-empty marker.
    door.write_text('#!/bin/sh\nprintf "%s" "$$" > "$HOME/door-$1.pid.tmp"\nmv "$HOME/door-$1.pid.tmp" "$HOME/door-$1.pid"\nexec /bin/sleep 600\n')
    door.chmod(0o700)

    def unprotected(pid, pane_id=None):
        session_leader = int(pathlib.Path(f'/proc/{pid}/stat').read_text().rsplit(')', 1)[1].split()[3])
        for action in (False, True):
            value = report(jail, action)
            matches = [p for p in value['panes'] if (pane_id is not None and p['pane_id'] == pane_id) or p.get('process', {}).get('pid') == session_leader]
            assert matches, ('hidden live PTY', pid, pane_id, pathlib.Path(f'/proc/{pid}/stat').read_text(),
                             value.get('pty_enumeration'),
                             [(p['pane_id'], jail.api('pane.process_info', {'pane_id': p['pane_id']})['process_info']['shell_pid']) for p in value['panes'] if p['pane_id']], value['panes'])
            assert all(p['status'] == 'UNPROTECTED' and p['reason'] for p in matches)
            assert len(value['coverage']) == 14 and all(row['status'] for row in value['coverage'])
        assert live(pid), 'census must not destroy unprotected process'

    before = {p['pane_id'] for p in jail.api('pane.list')['panes']}
    jail.api('layout.apply', {'workspace_id': 'w1', 'root': {'type': 'pane', 'command': [str(door), 'layout']}})
    explicit = wait(lambda: next((p['pane_id'] for p in jail.api('pane.list')['panes'] if p['pane_id'] not in before), None))
    pid = wait(lambda: int((jail.root / 'door-layout.pid').read_text()))
    unprotected(pid, explicit)
    with (jail.plugin / 'herdr-plugin.toml').open('a') as output:
        output.write('\n[[panes]]\nid = "dummy"\ntitle = "Dummy launch census"\ncommand = ["' + str(door) + '", "plugin"]\n')
    jail.api('plugin.link', {'path': str(jail.plugin)})
    for placement in ('split', 'tab', 'overlay', 'zoomed', 'popup'):
        marker = jail.root / 'door-plugin.pid'
        if marker.exists():
            marker.unlink()
        params = {'plugin_id': 'herdr-shield', 'entrypoint': 'dummy', 'placement': placement}
        if placement in ('split', 'zoomed'):
            params['target_pane_id'] = pane
        elif placement == 'tab':
            params['workspace_id'] = 'w1'
        result = jail.api('plugin.pane.open', params)
        pid = wait(lambda: int(marker.read_text()))
        plugin_pane = result.get('plugin_pane', {}).get('pane', {}).get('pane_id')
        unprotected(pid, plugin_pane)
        print('PASS census explicit plugin', placement, pid, flush=True)
        if placement == 'popup':
            jail.api('popup.close')
        else:
            jail.api('plugin.pane.close', {'pane_id': plugin_pane})
        wait(lambda: not live(pid))
    # Custom pane is a shell-command overlay/split; popup has no public pane ID.
    with config.open('a') as output:
        for key_name, kind in [('g', 'pane'), ('h', 'popup')]:
            output.write('\n[[keys.command]]\nkey = "prefix+' + key_name + '"\ntype = "' + kind + '"\ncommand = "' + str(door) + ' custom-' + kind + '"\n')
    jail.api('server.reload_config')
    jail.api('pane.focus', {'pane_id': pane})
    jail.terminate(jail.processes[1][0])
    jail.client()
    master = jail.clients[-1][0]
    wait(lambda: b'READY' in jail.client_output[-1])
    for key_name, kind in [(b'g', 'pane'), (b'h', 'popup')]:
        before = {p['pane_id'] for p in jail.api('pane.list')['panes']}
        key(key_name)
        pid = wait(lambda: int((jail.root / ('door-custom-' + kind + '.pid')).read_text()))
        new = [p['pane_id'] for p in jail.api('pane.list')['panes'] if p['pane_id'] not in before]
        unprotected(pid, new[0] if new else None)
        print('PASS census custom', kind, pid, flush=True)
        jail.api('popup.close') if kind == 'popup' else jail.api('pane.close', {'pane_id': new[0]})
        wait(lambda: not live(pid))
    print('PASS census preexisting, UI/default-shell, JSON doors, layout explicit, plugin split/tab/overlay/zoomed/popup, custom shell pane/popup: both list surfaces retain reasons', flush=True)
    return server, protected


def streams_and_refusals(jail, pane, meta):
    relay = jail.root / 'r/herdr-shield' / meta['id'] / 'herdr-api.sock'
    # Each refusal must be a typed error while the actual upstream remains alive.
    cases = [('unknown.method', {}), ('server.stop', {}), ('server.live_handoff', {}),
             ('server.reload_config', {}), ('server.reload_agent_manifests', {}),
             ('server.ssh_agent.register', {'socket_path': '/unavailable-agent.sock'}),
             ('client_shell.surface.set', {'active': True}), ('agent.explain', {'target': pane}),
             ('integration.install', {'target': 'codex'}), ('integration.uninstall', {'target': 'codex'}),
             ('plugin.link', {'path': str(jail.plugin)}), ('plugin.unlink', {'plugin_id': 'herdr-shield'}),
             ('plugin.enable', {'plugin_id': 'herdr-shield'}), ('plugin.disable', {'plugin_id': 'herdr-shield'})]
    connection = Stream(jail, relay)
    try:
        for index, (method, params) in enumerate(cases):
            connection.send(method, params, str(index))
            response = connection.read()
            assert response.get('id') == str(index) and response.get('error', {}).get('code') == 'unsupported_api', response
            assert 'result' not in response, response
        for data in (b'{broken}\n', b'{"id":"x","method":"ping","method":"server.stop","params":{}}\n', b'{"id":"x","method":"ping","params":{},"extension":true}\n'):
            connection.socket.sendall(data)
            assert connection.read()['error']['code'] == 'invalid_frame'
        connection.send('ping', {}, 'after-refusal')
        assert connection.read()['result']['protocol'] == 22
    finally:
        connection.close()
    connection = Stream(jail, relay)
    try:
        connection.socket.sendall(b'\x05\0\0\0\x16')
        assert connection.read()['error']['code'] == 'invalid_frame'
    finally:
        connection.close()
    print('PASS live relay refusal matrix: every admin/plugin-write/untyped/unknown method, duplicate/malformed/extension, binary; upstream still alive', flush=True)
    first, second = Stream(jail, relay), Stream(jail, relay)
    try:
        first.send('events.subscribe', {'subscriptions': [{'type': 'pane.output_matched', 'pane_id': pane, 'source': 'visible', 'match': {'type': 'substring', 'value': 'COUNTER=1'}}]}, 'subscribe-one')
        assert first.read() == {'id': 'subscribe-one', 'result': {'type': 'subscription_started'}}
        second.send('pane.wait_for_output', {'pane_id': pane, 'source': 'visible', 'match': {'type': 'substring', 'value': 'COUNTER=1'}, 'timeout_ms': 5000}, 'wait-one')
        jail.api('pane.send_text', {'pane_id': pane, 'text': 'tick\r'})
        event = first.read()
        assert event['event'] == 'pane.output_matched' and event['data']['pane_id'] == pane, event
        result = second.read()
        assert result['id'] == 'wait-one' and result['result']['type'] == 'output_matched', result
        assert second.file.readline() == b'', 'completed native wait must close its connection'
        second.close()
        second = Stream(jail, relay)
        # Closing one subscription cancels its upstream without affecting the other.
        first.close()
        second.send('events.subscribe', {'subscriptions': [{'type': 'workspace.created'}]}, 'subscribe-two')
        assert second.read()['result']['type'] == 'subscription_started'
        jail.api('workspace.create', {'label': 'stream-independent'})
        assert second.read()['event'] == 'workspace_created'
        replacement = Stream(jail, relay)
        try:
            replacement.send('events.wait', {'match_event': {'event': 'pane_agent_status_changed', 'pane_id': pane, 'agent_status': 'working'}, 'timeout_ms': 5000}, 'event-wait')
            jail.api('pane.report_agent', {'pane_id': pane, 'source': 'custom:stream-dummy', 'agent': 'dummy', 'state': 'working', 'seq': 1})
            value = replacement.read()
            assert value.get('id') == 'event-wait' and value.get('result', {}).get('type') == 'wait_matched', value
        finally:
            replacement.close()
        # Each cancelled wait owns only its own upstream connection.
        holder_fds = pathlib.Path(f"/proc/{meta['holder']['pid']}/fd")
        before_cancel = len(list(holder_fds.iterdir()))
        for index in range(8):
            cancelled = Stream(jail, relay)
            cancelled.send('events.wait', {'match_event': {'event': 'pane_agent_status_changed', 'pane_id': pane, 'agent_status': 'blocked'}, 'timeout_ms': 5000}, 'cancel-' + str(index))
            cancelled.close()
        wait(lambda: len(list(holder_fds.iterdir())) <= before_cancel + 2)
        moved = jail.api('pane.move', {'pane_id': pane, 'destination': {'type': 'new_workspace'}, 'focus': True})['move_result']['pane']['pane_id']
        wait(lambda: next(m for m in jail.metadata() if m['id'] == meta['id'])['binding']['pane'] == moved)
        # A later event proves the old generation cannot continue its stream.
        jail.api('workspace.create', {'label': 'generation-bound'})
        failure = second.read()
        if failure.get('event') == 'workspace_created':
            failure = second.read()
        assert failure['error']['code'] == 'stale_generation', failure
        assert jail.api('pane.get', {'pane_id': pane}, endpoint=relay)['pane']['pane_id'] == moved
    finally:
        if first.socket.fileno() >= 0:
            first.close()
        second.close()
    print('PASS streams: independent subscriptions/waits, cancel, reconnect, canonical events/results and generation-bound stale refusal', flush=True)


def recreated_session_does_not_close_retained_holder(jail, server, retained):
    pane, meta = retained
    original = identity(meta['child']['pid'])
    jail.terminate(server, signal.SIGKILL)
    for process, _, role in jail.processes:
        if role == 'client':
            jail.terminate(process)
    snapshot = jail.root / 'c/herdr/session.json'
    if snapshot.exists():
        snapshot.rename(snapshot.with_suffix('.previous-instance'))
    replacement = jail.server()
    jail.client()
    wait(lambda: jail.api('pane.list')['panes'])
    for _ in range(16):
        if any(value['pane_id'] == pane for value in jail.api('pane.list')['panes']):
            break
        jail.api('tab.create', {'workspace_id': 'w1'})
    else:
        raise AssertionError('did not recreate the retained public pane ID')
    wait(lambda: next(p for p in report(jail)['panes'] if p['pane_id'] == pane)['status'] == 'UNPROTECTED')
    assert identity(meta['child']['pid'])[:2] == original[:2]
    jail.api('pane.close', {'pane_id': pane})
    wait(lambda: not any(value['pane_id'] == pane for value in jail.api('pane.list')['panes']))
    for _ in range(10):
        jail.keep('reconcile')
        assert identity(meta['child']['pid'])[:2] == original[:2], 'reused ID close killed previous session workload'
    current = next(value for value in jail.metadata() if value['id'] == meta['id'])
    assert current['binding']['server'] == meta['binding']['server']
    assert current['binding']['server']['pid'] != replacement.pid
    print('PASS destructive session recreation/ID reuse: closing new pane and repeated reconciliation retain exact old child/holder and old server binding', flush=True)


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument('--herdr', required=True)
    parser.add_argument('--keeper', required=True)
    parser.add_argument('--root', type=pathlib.Path, required=True)
    args = parser.parse_args()
    jail = Jail(args.root, str(pathlib.Path(args.herdr).resolve()), str(pathlib.Path(args.keeper).resolve()))
    try:
        server, protected = census(jail)
        pane, meta = protected[0]
        streams_and_refusals(jail, pane, meta)
        assert live(server.pid)
        recreated_session_does_not_close_retained_holder(jail, server, protected[1])
    finally:
        jail.close()


if __name__ == '__main__':
    main()
