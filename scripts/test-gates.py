#!/usr/bin/env python3
"""Prove formatting, lint and assertion gates reject broken production-source copies."""
import os
import pathlib
import shutil
import socket
import subprocess
import tempfile
import sys

sys.path.insert(0, str(pathlib.Path(__file__).resolve().parent))
from gate_run import run

source = pathlib.Path(__file__).resolve().parents[1]
if not os.environ.get('HERDR_SHIELD_BUILD_ROOT') or not os.environ.get('TMPDIR'):
    sys.exit('ERROR: export HERDR_SHIELD_BUILD_ROOT and TMPDIR for the whole isolated verification sequence')
parent = pathlib.Path(os.environ['HERDR_SHIELD_BUILD_ROOT']).resolve(strict=True)
assert parent.is_absolute() and parent.is_relative_to(pathlib.Path(os.environ['TMPDIR']))
scratch = pathlib.Path(tempfile.mkdtemp(prefix='gc-', dir=parent))
temporary = scratch / 'tmp'
temporary.mkdir(mode=0o700)
environment = dict(os.environ, TMPDIR=str(temporary), CARGO_TARGET_DIR=str(parent / 'gate-control-target'))
for name in ('Cargo.toml', 'Cargo.lock', 'rust-toolchain.toml'):
    shutil.copyfile(source / name, scratch / name)
for directory in ('src', 'tests'):
    shutil.copytree(source / directory, scratch / directory)
target = scratch / 'src/main.rs'
original = target.read_text()
controls = [
    ('format', '\nfn broken_format_control( ){ }\n', ['cargo', 'fmt', '--all', '--', '--check'], 'Diff in'),
    ('lint', '\n#[allow(dead_code)]\nfn broken_lint_control() { let _ = true == true; }\n',
     ['cargo', 'clippy', '--offline', '--locked', '--bin', 'herdr-shield', '--', '-D', 'warnings'], 'equal expressions'),
    ('assertion', '\n#[test]\nfn broken_assertion_control() { assert!(false, "BROKEN_ASSERTION_CONTROL"); }\n',
     ['cargo', 'test', '--offline', '--locked', '--bin', 'herdr-shield', 'broken_assertion_control', '--', '--nocapture'], 'BROKEN_ASSERTION_CONTROL'),
]
try:
    for name, mutation, command, expected in controls:
        target.write_text(original + mutation)
        result = subprocess.run(command, cwd=scratch, env=environment, stdout=subprocess.PIPE, stderr=subprocess.STDOUT,
                                text=True, timeout=120)
        (scratch / (name + '.log')).write_text(result.stdout)
        assert result.returncode != 0 and expected in result.stdout, (name, result.returncode, result.stdout)
        print('CONTROL rejected:', name, 'actual production-source mutation', flush=True)
    target.write_text(original)
    for name, relative, old, new, test, expected in [
        ('capture-worker-bound', 'src/herdr/resume.rs', 'if active >= 4 {', 'if active >= 5 {',
         'persisted_slot_and_epoch_not_ok_control_capture_and_partial_mutation', 'four independently observed worker threads'),
        ('identity-mapping', 'src/herdr/mapping.rs', 'self.direct(params)?;', '// mutation: omit direct mapping',
         'closed_identity_field_and_nested_request_matrix_preserves_opaque_values', 'direct identity'),
        ('private-permissions', 'src/config.rs', 'meta.mode() & 0o077 != 0', 'false',
         'private_paths_reject_symlinks_public_modes_and_corrupt_json', 'secure_dir(&public).is_err()'),
        ('atomic-write-cleanup', 'src/config.rs', 'if result.is_err() {', 'if false {',
         'atomic_write_failure_preserves_old_record_and_removes_staging_files', 'staging file survived failed serialization'),
        ('pty-census', 'src/herdr/census.rs', 'result.push(identity);', '// mutation: omit PTY owner',
         'census_sees_an_independently_owned_real_pty_session', 'real PTY omitted'),
        ('unsupported-refusal', 'src/unsupported.rs', 'UNPROTECTED: Linux', 'UNSAFE: Linux',
         'unsupported_platform_entrypoint_runs_plain_shell_and_reports_unprotected', 'UNPROTECTED: Linux'),
        ('peer-uid', 'src/holder/protocol.rs', 'if uid != Uid::current().as_raw() {', 'if false {',
         'kernel_peer_credentials_and_different_uid_refusal', 'unwrap_err'),
    ]:
        path = scratch / relative
        saved = path.read_text()
        assert old in saved, ('mutation anchor absent', name)
        path.write_text(saved.replace(old, new, 1))
        result = subprocess.run(['cargo', 'test', '--offline', '--locked', '--bin', 'herdr-shield', test, '--', '--nocapture'],
                                cwd=scratch, env=environment, stdout=subprocess.PIPE, stderr=subprocess.STDOUT, text=True, timeout=120)
        (scratch / (name + '.log')).write_text(result.stdout)
        path.write_text(saved)
        assert result.returncode != 0 and expected in result.stdout, (name, result.returncode, result.stdout)
        print('CONTROL rejected:', name, 'actual production-source mutation', flush=True)
    for name, relative, old, new, test, expected in [
        ('query-delivery-regression', 'src/holder/state.rs', 'let query_writer = query_sender;', 'let query_writer = writer.clone();',
         'saturated_detached_query_replies_are_delivered_exactly_once', 'assertion `left == right` failed'),
        ('recursive-shell-regression', 'src/attach/mod.rs', 'return recovery::plain(&config, "recursive keeper refused");',
         'return fail("recursive keeper refused");', 'failed_manager_cgroup_native_resume_and_recursive_launch_open_plain_shell', 'recursive:'),
    ]:
        path = scratch / relative
        saved = path.read_text()
        assert old in saved, ('mutation anchor absent', name)
        path.write_text(saved.replace(old, new, 1))
        result = run(['cargo', 'test', '--offline', '--locked', '--test', 'process', test, '--', '--nocapture'],
                     cwd=scratch, env=dict(environment, TMPDIR=str(parent), HERDR_SHIELD_TEST_ROOT=str(parent / 'g')),
                     timeout=120)
        (scratch / (name + '.log')).write_text(result.stdout)
        path.write_text(saved)
        assert result.returncode != 0 and expected in result.stdout and 'CLEANUP process survivors: []' in result.stdout, result.stdout
        print('CONTROL rejected:', name, 'actual production-source mutation; zero owned survivors', flush=True)
finally:
    target.write_text(original)
    for path in temporary.rglob('*.sock'):
        assert path.is_relative_to(temporary) and not path.is_symlink() and path.is_socket()
        with socket.socket(socket.AF_UNIX) as connection:
            connection.settimeout(1)
            try:
                connection.connect(str(path))
            except ConnectionRefusedError:
                path.unlink()
                print('CLEANUP failed-control socket', path.name, flush=True)
            else:
                raise AssertionError('broken control retained a live socket: ' + str(path))
print('PASS gate controls: fmt, clippy and test assertions reject broken production-source artifacts', flush=True)
