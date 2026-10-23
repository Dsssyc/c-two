from pathlib import Path
import hashlib, json, os, shutil, subprocess, sys, threading

repo = Path.cwd()
sys.path.insert(0, str(repo))
from tools.dev.test_python import run_step

root = Path(os.environ['RUNNER_TEMP']) / 'r13-arm-installer-probe'
root.mkdir()
out = repo / 'artifacts/arm-installer-probe'
out.mkdir(parents=True)
python = os.environ['PROBE_PYTHON']
base = '3e575acbb4c30414f06caaeb7e6e8c84e503ff3c'
candidate = '1ac341506acbafcd28382deae291359abbf03aa9'
paths = ['cli/install-c3.ps1', 'tests/repo/test_install_c3_powershell.py']
identity = {}
for phase, rev in [('baseline', base), ('candidate', candidate)]:
    identity[phase] = {}
    for name in paths:
        data = subprocess.check_output(['git', 'show', f'{rev}:{name}'])
        path = root / phase / name
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_bytes(data)
        identity[phase][name] = hashlib.sha256(data).hexdigest()
assert identity['baseline'] == identity['candidate']
(out / 'source-identity.json').write_text(json.dumps(identity, indent=2))
rows = []
env = dict(os.environ, PYTEST_ADDOPTS='')

def execute(label, command, timeout=45):
    def snapshot():
        code = "Get-CimInstance Win32_Process | Where-Object { $_.Name -in @('powershell.exe','pwsh.exe','csc.exe','conhost.exe','python.exe') } | Select-Object ProcessId,ParentProcessId,Name,CommandLine | ConvertTo-Json -Depth 3"
        with (out / (label + '-processes.json')).open('w') as log:
            try:
                subprocess.run(['pwsh', '-NoProfile', '-NonInteractive', '-Command', code], stdout=log, stderr=subprocess.STDOUT, timeout=15)
            except subprocess.TimeoutExpired:
                log.write('\nprocess snapshot timed out\n')
    timer = threading.Timer(10, snapshot)
    timer.start()
    try:
        row = run_step(command, out / label, env, timeout)
    finally:
        timer.cancel()
        timer.join()
    rows.append({'label': label, **row})
    (out / 'results.json').write_text(json.dumps(rows, indent=2))
    print(label, row['status'], row['exit_code'], row['duration_seconds'], flush=True)

for shell in ['SysWOW64', 'System32', 'pwsh']:
    for phase in ['baseline', 'candidate']:
        target = root / phase / paths[1]
        execute(f'{phase}-{shell}', [python, '-m', 'pytest', f'{target}::test_powershell_selects_native_os_target[{shell}]', '-vv', '-s', '--timeout=30', '--junitxml=' + str(out / f'{phase}-{shell}.xml')])

syswow = str(Path(os.environ['SystemRoot']) / 'SysWOW64/WindowsPowerShell/v1.0/powershell.exe')
empty = root / 'empty.ps1'
empty.write_text("[Console]::WriteLine('host-ready'); exit 0\n")
execute('SysWOW64-empty', [syswow, '-NoProfile', '-ExecutionPolicy', 'Bypass', '-File', str(empty)], 30)
source = (root / 'baseline/cli/install-c3.ps1').read_text()
phase_log = out / 'SysWOW64-phases.txt'
marker = "[System.IO.File]::AppendAllText($env:C2_PHASE_LOG, ('{0:o} {1}' -f [DateTime]::UtcNow, '{phase}') + [Environment]::NewLine)"
source = source.replace('$ErrorActionPreference = "Stop"', marker.replace('{phase}', 'script-start') + '\n$ErrorActionPreference = "Stop"')
source = source.replace('            Add-Type -Namespace', '            ' + marker.replace('{phase}', 'before-Add-Type') + '\n            Add-Type -Namespace')
source = source.replace('        [System.UInt16]$processMachine = 0', '        ' + marker.replace('{phase}', 'after-Add-Type') + '\n        [System.UInt16]$processMachine = 0')
source = source.replace('        if (-not [C3Installer.NativeSystem]', '        ' + marker.replace('{phase}', 'before-native-call') + '\n        if (-not [C3Installer.NativeSystem]')
source = source.replace('        switch ($nativeMachine)', '        ' + marker.replace('{phase}', 'after-native-call') + '\n        switch ($nativeMachine)')
source = source.replace('if ($PrintTarget) {', marker.replace('{phase}', 'target-resolved') + '\nif ($PrintTarget) {')
probe = root / 'instrumented.ps1'
probe.write_text(source)
env['C2_PHASE_LOG'] = str(phase_log)
execute('SysWOW64-instrumented', [syswow, '-NoProfile', '-ExecutionPolicy', 'Bypass', '-File', str(probe), '-PrintTarget'], 30)
print('Diagnostic collection complete; inspect individual outcomes, not this driver exit code.', flush=True)
shutil.rmtree(root)
