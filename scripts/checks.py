#!/usr/bin/env python3
"""Apply the same structural validator to production files and mutated controls."""
import pathlib
import re
import subprocess
import sys

ROOT = pathlib.Path(__file__).resolve().parents[1]
FORBIDDEN = re.compile(r'(?i)' + 'pro' + 'fessor' + r'|\b' + 'p' + 'fm' + r'\b|/ho' + r'me/|/U' + r'sers/|herdr[-_]' + 'keep')


def audit(files):
    errors = []
    rust = {name: text for name, text in files.items() if name.endswith('.rs')}
    if not rust:
        return ['source enumeration was empty']
    for name, text in files.items():
        if name.endswith('.rs') and len(text.splitlines()) > 800:
            errors.append('source exceeds 800 lines: ' + name)
        if name.startswith('docs/') and (len(text.splitlines()) > 500 or len(text.encode()) > 81920):
            errors.append('doc exceeds topic limit: ' + name)
        if FORBIDDEN.search(text):
            errors.append('forbidden origin or machine path: ' + name)
        if name.startswith('src/') and name.endswith('.rs'):
            if re.search(r'(?:std::process::)?Command::new\s*\(', text) and name != 'src/shell.rs':
                errors.append('process facade bypass: ' + name)
            if re.search(r'std::env::var(?:_os)?\("(?:HOME|XDG_[A-Z_]+)"\)', text) and name not in ('src/config.rs', 'src/lifetime/systemd.rs', 'src/main.rs'):
                errors.append('path facade bypass: ' + name)
            if '/schema/' not in name and not re.search(r'#\[cfg\((?:all\()?test[,)]', text) and pathlib.Path(name).name not in ('main.rs', 'mod.rs'):
                errors.append('no owning unit test: ' + name)
    if not any('#[test]' in text for name, text in rust.items() if name.startswith('tests/')):
        errors.append('process test enumeration was empty')
    source = files.get('src/herdr/schema/mod.rs', '')
    methods = re.findall(r'#\[serde\(rename = "([^"]+)"\)\]', source)
    if len(methods) != 102 or len(set(methods)) != 102:
        errors.append('pinned method enumeration did not yield exactly 102 unique variants')
    policies = files.get('src/herdr/policy.rs', '')
    for method in methods:
        if len(re.findall(r'"' + re.escape(method) + r'"', policies)) != 1:
            errors.append('method policy missing/duplicated: ' + method)
    main = files.get('src/main.rs', '')
    for command in re.findall(r'\(\s*"([a-z][a-z-]*)(?: [^"]*)?",', main.split('fn main()', 1)[0]):
        if 'Some("' + command + '")' not in main:
            errors.append('registry command has no CLI route: ' + command)
    manifest = files.get('herdr-plugin.toml', '')
    for command in re.findall(r'command = \["bin/herdr-shield", "([^"]+)"\]', manifest):
        if not re.search(r'\(\s*"' + re.escape(command) + r'",', main):
            errors.append('manifest command absent from registry: ' + command)
    return errors


def published():
    """Every text file git would publish: tracked, plus untracked files no ignore rule hides."""
    listed = subprocess.run(['git', 'ls-files', '-z', '--cached', '--others', '--exclude-standard'],
                            cwd=ROOT, capture_output=True, check=False)
    if listed.returncode != 0:
        sys.exit('ERROR: could not enumerate published files: git ls-files exited '
                 f'{listed.returncode}: {listed.stderr.decode(errors="replace").strip()}')
    files = {}
    for name in sorted(set(listed.stdout.decode().split('\0')) - {''}):
        path = ROOT / name
        if not path.is_file():
            continue  # deleted in the working tree: it ships no content
        data = path.read_bytes()
        if b'\0' not in data:  # an image or other binary carries no text line to scan
            files[name] = data.decode()
    return files


def main():
    files = published()
    errors = audit(files)
    if errors:
        sys.exit('\n'.join('ERROR: ' + error for error in errors))
    controls = [
        ('source enumeration', None, None),
        ('source exceeds', 'src/config.rs', 'line\n' * 801),
        ('doc exceeds', 'docs/broken-control.md', 'line\n' * 501),
        ('doc exceeds', 'docs/broken-control.md', 'x' * 81921),
        ('forbidden origin', 'src/config.rs', files['src/config.rs'] + '\n//' + 'pro' + 'fessor'),
        ('forbidden origin', 'tests/broken.rs', '// /ho' + 'me/example'),
        ('forbidden origin', 'tests/broken.rs', '// ' + 'p' + 'fm'),
        ('forbidden origin', 'tests/broken.rs', '// /U' + 'sers/example'),
        ('forbidden origin', 'docs/broken-control.md', '`/tmp/' + 'pro' + 'fessor/evidence/`'),
        ('forbidden origin', 'tests/broken.rs', '// herdr-' + 'keep'),
        ('forbidden origin', 'tests/broken.rs', '// HERDR_' + 'KEEP_INNER'),
        *(('forbidden origin', name, files[name] + '\n/ho' + 'me/example\n')
          for name in ('README.md', 'CHANGELOG.md', 'releases/v0.1.0.md', 'CONTRIBUTING.md', 'CODE_OF_CONDUCT.md',
                       'SECURITY.md', 'SUPPORT.md', '.github/pull_request_template.md',
                       '.github/ISSUE_TEMPLATE/bug_report.md')),
        ('process facade bypass', 'src/terminal/render.rs', files['src/terminal/render.rs'] + '\nCommand::new("bad");'),
        ('path facade bypass', 'src/terminal/render.rs', files['src/terminal/render.rs'] + '\nstd::env::var_os("HOME");'),
        ('no owning unit test', 'src/terminal/render.rs', files['src/terminal/render.rs'].replace('#[cfg(test)]', '')),
        ('pinned method enumeration', 'src/herdr/schema/mod.rs', ''),
        ('method policy missing/duplicated', 'src/herdr/policy.rs', files['src/herdr/policy.rs'].replace('"ping"', '"removed"', 1)),
        ('method policy missing/duplicated', 'src/herdr/policy.rs', files['src/herdr/policy.rs'] + '\n"ping"'),
        ('manifest command absent', 'herdr-plugin.toml', files['herdr-plugin.toml'].replace('"reconcile"', '"broken-command"', 1)),
        ('registry command has no CLI route', 'src/main.rs', files['src/main.rs'].replace('Some("reconcile")', 'Some("unwired-command")', 1)),
    ]
    for expected, path, replacement in controls:
        changed = dict(files)
        if path is None:
            changed = {name: text for name, text in changed.items() if not name.endswith('.rs')}
        else:
            changed[path] = replacement
        failures = audit(changed)
        assert any(error.startswith(expected) for error in failures), ('broken control accepted', expected, failures)
        print('CONTROL rejected:', expected, path or '')
    changed = {name: text.replace('#[test]', '') if name.startswith('tests/') else text for name, text in files.items()}
    assert 'process test enumeration was empty' in audit(changed)
    print(f'PASS structure: {sum(name.endswith(".rs") for name in files)} Rust files; 102 unique policies; all {len(controls) + 1} actual artifact mutation controls rejected')


if __name__ == '__main__':
    main()
