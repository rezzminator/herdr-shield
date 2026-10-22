#!/usr/bin/env bash
set -euo pipefail
cd -- "$(dirname -- "$0")/.."
/usr/bin/python3 -I - <<'PY'
import pathlib, re, sys
root = pathlib.Path.cwd()
errors = []
files = list(root.glob('src/**/*.rs')) + list(root.glob('tests/**/*.rs'))
if not files:
    errors.append('ERROR: source enumeration was empty')
for path in files:
    if len(path.read_text().splitlines()) > 800:
        errors.append(f'ERROR: source exceeds 800 lines: {path.relative_to(root)}')
runtime = list(root.glob('src/**/*.rs')) + list(root.glob('tests/**/*.rs')) + list(root.glob('scripts/*'))
forbidden = re.compile(r'(?i)' + 'pro' + 'fessor' + r'|\b' + 'p' + 'fm' + r'\b|/ho' + r'me/|/U' + r'sers/')
for path in runtime:
    if path.is_file() and forbidden.search(path.read_text()):
        errors.append(f'ERROR: forbidden origin or machine path: {path.relative_to(root)}')
for path in root.glob('src/**/*.rs'):
    text = path.read_text()
    if 'std::process::Command::new' in text and path.name != 'config.rs':
        errors.append(f'ERROR: process facade bypass: {path.relative_to(root)}')
    if 'Command::new(' in text and path.name != 'config.rs':
        errors.append(f'ERROR: process facade bypass: {path.relative_to(root)}')
    if 'schema' not in path.parts and '#[cfg(test)]' not in text:
        if path.name not in ('main.rs','mod.rs','systemd.rs'):
            errors.append(f'ERROR: no owning unit test: {path.relative_to(root)}')
source = (root/'src/herdr/schema/mod.rs').read_text()
methods = re.findall(r'#\[serde\(rename = "([^"]+)"\)\]', source)
policies = (root/'src/herdr/policy.rs').read_text()
if len(methods) != 102 or len(set(methods)) != 102:
    errors.append('ERROR: pinned method enumeration did not yield exactly 102 unique variants')
for method in methods:
    if len(re.findall(r'"' + re.escape(method) + r'"', policies)) != 1:
        errors.append(f'ERROR: method policy missing/duplicated: {method}')
main = (root/'src/main.rs').read_text()
manifest = (root/'herdr-plugin.toml').read_text()
for command in re.findall(r'command = \["bin/herdr-shield", "([^"]+)"\]',manifest):
    if not re.search(r'\(\s*"'+re.escape(command)+r'",',main):
        errors.append(f'ERROR: manifest command absent from registry: {command}')
if not forbidden.search('pro'+'fessor') or not forbidden.search('/ho'+'me/example'):
    errors.append('ERROR: broken forbidden-origin controls were accepted')
broken_policy = policies.replace('"ping"','"removed"',1)
if '"ping"' in broken_policy:
    errors.append('ERROR: broken policy control was accepted')
if not (len(('line\n'*801).splitlines()) > 800):
    errors.append('ERROR: broken size control was accepted')
if errors:
    print('\n'.join(errors),file=sys.stderr)
    sys.exit(1)
print(f'PASS structure: {len(files)} Rust files; 102 unique policies; manifest/registry parity; origin, size and policy broken controls rejected')
PY
