#!/usr/bin/env python3
"""Exercise the actual UI radio workers through owner SSH; no installation or pairing."""
import argparse
import datetime
import json
from pathlib import Path
import shlex
import subprocess
import time


def validate_progress(samples):
    states = [s.get('scan', {}).get('state') for s in samples]
    if not any(s in ('starting', 'scanning') for s in states):
        raise RuntimeError('no active scan state observed')
    if not samples or states[-1] != 'complete':
        raise RuntimeError('scan did not complete: ' + str(states[-1:]))
    if not isinstance(samples[-1]['scan'].get('found'), int):
        raise RuntimeError('scan result count missing')
    return {'passed': True, 'states': list(dict.fromkeys(states)), 'found': samples[-1]['scan']['found']}


def main():
    p = argparse.ArgumentParser(description=__doc__)
    p.add_argument('--host', default='root@10.42.0.1')
    p.add_argument('--identity', type=Path, default=Path.home()/'.ssh/y2linux_ed25519')
    p.add_argument('--known-hosts', type=Path, required=True)
    p.add_argument('--expected-build', required=True)
    p.add_argument('--output', type=Path, default=Path('out/radio-ui-qualification'))
    a = p.parse_args()
    if not a.known_hosts.is_file(): p.error('existing owner-approved host pin required')
    out = a.output / datetime.datetime.now(datetime.timezone.utc).strftime('%Y%m%dT%H%M%SZ')
    out.mkdir(parents=True, exist_ok=False)
    ssh = ['ssh', '-i', str(a.identity), '-o', 'BatchMode=yes', '-o', 'StrictHostKeyChecking=yes',
           '-o', f'UserKnownHostsFile={a.known_hosts}', '-o', 'ConnectTimeout=5', a.host]
    sequence = 0

    def call(*args):
        nonlocal sequence
        sequence += 1
        r = subprocess.run(ssh + [shlex.join(['rebornctl', *args, '--json'])], capture_output=True, timeout=10)
        value = json.loads(r.stdout)
        (out / f'{sequence:03d}-{args[0]}.json').write_text(json.dumps(value, indent=2)+'\n')
        if not isinstance(value, dict) or r.returncode or value.get('ok') is False:
            raise RuntimeError(f'{args}: {value}')
        return value

    results = {}; failures = []
    try:
        before = call('status')
        if before['build_id'] != a.expected_build: raise RuntimeError('unexpected build; no radio changes made')
        if any(before[r].get('scan', {}).get('state') in ('starting', 'scanning') for r in ('wifi', 'bluetooth')):
            raise RuntimeError('a UI scan is already active; no radio changes made')
        for radio, power_field in [('wifi', 'enabled'), ('bluetooth', 'powered')]:
            samples = []
            try:
                call(radio, 'scan')
                deadline = time.monotonic() + 40
                while time.monotonic() < deadline:
                    status = call('status')
                    if status['session'] != before['session']: raise RuntimeError('Reborn restarted')
                    samples.append(status[radio])
                    if samples[-1].get('scan', {}).get('state') in ('complete', 'failed'): break
                    time.sleep(.25)
                results[radio] = validate_progress(samples)
            finally:
                # Scan is an explicit user operation and remembers On. Restore
                # Off only if this qualification enabled the previously-off radio.
                if not before[radio][power_field]:
                    call(radio, 'off')
                    deadline = time.monotonic() + 10
                    while call('status')[radio][power_field]:
                        if time.monotonic() >= deadline: raise RuntimeError(f'{radio}: power restoration timed out')
                        time.sleep(.25)
        final = call('status')
        if final['session'] != before['session'] or final['build_id'] != a.expected_build:
            raise RuntimeError('Reborn identity changed')
        call('snapshot')
    except (RuntimeError, OSError, KeyError, ValueError, subprocess.TimeoutExpired) as error:
        failures.append(str(error))
    summary = {'passed': not failures, 'results': results, 'failures': failures,
               'physical_acceptance': False, 'manual_remaining': ['visible scan progress/results', 'discoverable Bluetooth peer', 'pairing/network connection']}
    (out/'summary.json').write_text(json.dumps(summary, indent=2)+'\n')
    print(json.dumps({'directory': str(out), **summary}))
    return 0 if summary['passed'] else 1


if __name__ == '__main__': raise SystemExit(main())
