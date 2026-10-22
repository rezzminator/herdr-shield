#!/usr/bin/env python3
"""Real accepted-state and holder-loss tests using only an owned dummy process."""
import argparse
import json
import os
import pathlib
import pty
import select
import signal
import subprocess
import sys
import time

sys.path.insert(0, str(pathlib.Path(__file__).resolve().parent))
from real import Jail, identity, live, wait


def resume_test(jail):
    script = jail.root / 'dummy-agent.py'
    script.write_bytes((pathlib.Path(__file__).resolve().parents[1] / 'fakes/dummy-agent.py').read_bytes())
    jail.server()
    jail.client()
    pane = wait(lambda: jail.api('pane.list')['panes'])[0]['pane_id']
    meta = wait(lambda: next((m for m in jail.metadata() if m['binding']['pane'] == pane and m['state'] == 'running'), None))
    wait(lambda: (jail.root / 'c/herdr/session.json').is_file())
    jail.api('pane.send_text', {'pane_id': pane, 'text': 'agent\r'})
    wait(lambda: 'DUMMY_AGENT_READY' in jail.api('pane.read', {'pane_id': pane, 'source': 'visible'})['read']['text'])
    def current():
        return next(m for m in jail.metadata() if m['id'] == meta['id'])
    count = 0
    def report(source, agent, seq, argv=None, method='pane.report_agent', helper=False, **extra):
        nonlocal count
        count += 1
        params = dict(pane_id=pane, source=source, agent=agent, seq=seq, **extra)
        if method == 'pane.report_agent':
            params['state'] = 'idle'
        if argv is not None:
            params['resume_argv'] = argv
        request = dict(id=str(count), method=method, params=params)
        jail.api('pane.send_text', {'pane_id': pane, 'text': json.dumps(dict(request=request, helper=helper)) + '\r'})
        logs = wait(lambda: (jail.root / 'responses.jsonl').read_text().splitlines() if (jail.root / 'responses.jsonl').exists() else [])
        logs = wait(lambda: lines if len(lines := (jail.root / 'responses.jsonl').read_text().splitlines()) >= count else None)
        result = json.loads(logs[count - 1])
        assert result['response']['id'] == str(count)
        return result
    argv = ['python3', str(script), 'resumed', 'a b', '', '界']
    accepted = report('custom:dummy', 'python3', 1, argv, helper=True)
    assert accepted['response']['result']['type'] == 'ok', accepted
    captured = wait(lambda: current().get('resume'))
    assert captured['argv'] == argv, captured
    assert captured.get('process') is not None, 'accepted local original process must have independent provenance'
    assert captured['process']['pid'] == meta['child']['pid'], captured
    assert accepted['pid'] != captured['process']['pid'], 'helper was substituted for original process'
    assert identity(captured['process']['pid'])[:2] == (captured['process']['pid'], str(captured['process']['start']))
    print('PASS accepted custom resume: exact argv; foreground root boot/start proof, not reporting helper', flush=True)
    newer = report('custom:dummy', 'python3', 3)
    assert newer['response']['result']['type'] == 'ok'
    stale = report('custom:dummy', 'python3', 2, ['python3', str(script), 'resumed', 'STALE'])
    assert stale['response']['result']['type'] == 'ok'
    assert current().get('resume') is None or current()['resume']['argv'] == argv, current()
    print('PASS newer no-argv followed by stale argv: native ok unchanged; no stale fallback capture', flush=True)
    refused = report('custom:intruder', 'intruder', 1, ['python3', str(script), 'resumed', 'INTRUDER'], method='pane.report_agent_session')
    assert refused['response']['error']['code'] == 'resume_not_accepted', refused
    assert current().get('resume') is None or current()['resume']['argv'] == argv
    native = report('herdr:codex', 'codex', 1, ['python3', str(script), 'resumed', 'NATIVE'], method='pane.report_agent_session', agent_session_id='dummy-native-session')
    assert native['response'].get('error', {}).get('code') == 'resume_not_accepted', native
    assert current().get('resume') is None or current()['resume']['argv'] == argv
    print('PASS native/custom arbitration refusal remains upstream error, never false argv capture', flush=True)
    def native_partial():
        snapshot=json.loads((jail.root/'c/herdr/session.json').read_text())
        return next((value for workspace in snapshot['workspaces'] for tab in workspace['tabs'] for value in tab['panes'].values() if value.get('agent_session',{}).get('value')=='dummy-native-session'),None)
    partial=wait(native_partial)
    assert partial['agent_session']['source']=='herdr:codex'
    assert partial['agent_resume']['argv']==argv,partial
    print('PASS real partial mutation: native error still persisted session identity; rejected native argv never became accepted fallback',flush=True)
    ssh_argv = ['ssh', 'fixture.invalid', 'dummy-agent', 'resume', 'remote-session']
    accepted = report('custom:dummy', 'python3', 4, ssh_argv, helper=True)
    assert accepted['response']['result']['type'] == 'ok', accepted
    remote = wait(lambda: value if (value := current().get('resume')) and value['argv'] == ssh_argv else None)
    assert remote['process'] is None, 'local SSH-style reporting cannot prove remote workload death'
    print('PASS SSH-style accepted resume capture: remote argv preserved, local reporting helper never supplies remote process-death proof', flush=True)
    # Re-establish a deliberately accepted custom command after native mutation.
    accepted = report('custom:dummy', 'python3', 5, argv + ['FINAL'], helper=True)
    assert accepted['response']['result']['type'] == 'ok', accepted
    argv += ['FINAL']
    captured = wait(lambda: value if (value := current().get('resume')) and value['argv'] == argv else None)
    assert captured['process']['pid'] == meta['child']['pid']
    config = jail.root / 'c/herdr-shield/config.json'
    config.write_text(json.dumps({'shell': '/bin/sh', 'args': ['-c', 'printf PLAIN_CHOICE']}))
    config.chmod(0o600)
    def choose(key, allowed):
        master,slave=pty.openpty()
        process=subprocess.Popen([jail.keeper,'attach',meta['id']],env=jail.env,cwd=jail.root,stdin=slave,stdout=slave,stderr=slave,start_new_session=True)
        os.close(slave)
        jail.processes.append((process,identity(process.pid),'recovery'))
        output=bytearray()
        phrase=b'Enter runs the exact argv once' if allowed else b'Resume is not offered'
        def read():
            if select.select([master],[],[],0.1)[0]:
                try:output.extend(os.read(master,65536))
                except OSError:pass
            return phrase in output
        try:
            wait(read)
            assert meta['id'].encode() in output
            assert json.dumps(argv,ensure_ascii=False,separators=(',',':')).encode() in output,output
            assert (b'Enter runs the exact argv once' in output)==allowed,output
            assert not (jail.root/'resumed.json').exists(),'automatic resume before key'
            os.write(master,key)
            process.wait(timeout=10)
            while select.select([master],[],[],0)[0]:
                try:output.extend(os.read(master,65536))
                except OSError:break
            if allowed and key==b'\r':
                assert json.loads((jail.root/'resumed.json').read_text())==argv[1:]
            else:
                assert b'PLAIN_CHOICE' in output,output
                assert not (jail.root/'resumed.json').exists()
        finally:
            jail.terminate(process)
            os.close(master)
    # Socket loss is not process death. Even Enter must select the plain shell.
    endpoint=pathlib.Path(jail.env['XDG_RUNTIME_DIR'])/'herdr-shield'/meta['id']/'holder.sock'
    held=endpoint.with_suffix('.held')
    record=jail.root/'s/herdr-shield'/f"{meta['id']}.json"
    saved=record.read_bytes()
    endpoint.rename(held)
    try:
        choose(b'\r',False)
        assert live(meta['child']['pid'])
        unknown=json.loads(saved);unknown['resume']['process']=None
        record.write_text(json.dumps(unknown))
        choose(b'\r',False)
        assert live(meta['child']['pid'])
        for name, patch in [('reused', {'start': captured['process']['start'] + 1}),
                            ('remote-ssh', {'boot': 'ssh:remote-dummy-session'}),
                            ('invalid-pid', {'pid': 0}), ('unknown-start', {'start': 0})]:
            value = json.loads(saved)
            value['resume']['process'].update(patch)
            record.write_text(json.dumps(value))
            choose(b'\r', False)
            assert live(meta['child']['pid']), name
            print('PASS recovery identity matrix:', name, 'suppresses Enter without execution', flush=True)
    finally:
        record.write_bytes(saved)
        held.rename(endpoint)
    print('PASS ambiguous socket loss: live and unverifiable original both suppress Enter; no holder killed',flush=True)
    # Stop only our recorded, uniquely prefixed test service. Independently prove
    # both the original root and holder dead before expecting an Enter offer.
    unit = current()['unit']
    assert unit.startswith('herdr-shield-test-') and unit in jail.units
    result = subprocess.run(['systemctl', '--user', f'--machine={os.getuid()}@.host', 'stop', unit], env=dict(jail.env, DBUS_SESSION_BUS_ADDRESS=f'unix:path=/run/user/{os.getuid()}/bus'),
                            stdout=subprocess.PIPE, stderr=subprocess.PIPE, timeout=15)
    assert result.returncode == 0, result.stderr
    wait(lambda: not live(meta['child']['pid']) and not live(meta['holder']['pid']))
    # Existing pane attachment also observes loss and must not auto-execute.
    time.sleep(0.2)
    assert not (jail.root / 'resumed.json').exists()
    choose(b'x',True)
    choose(b'\r',True)
    print('PASS missing holder: no automatic execution; other key plain shell; Enter executes exact argv once only after proven death', flush=True)


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument('--herdr', required=True)
    parser.add_argument('--keeper', required=True)
    parser.add_argument('--root', type=pathlib.Path, required=True)
    args = parser.parse_args()
    jail = Jail(args.root, str(pathlib.Path(args.herdr).resolve()), str(pathlib.Path(args.keeper).resolve()))
    try:
        resume_test(jail)
    finally:
        jail.close()


if __name__ == '__main__':
    main()
