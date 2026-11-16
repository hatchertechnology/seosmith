#!/usr/bin/env python3
"""Bound and measure one offline comparison on Linux, including process startup."""
import argparse
import json
import os
from pathlib import Path
import resource
import signal
import subprocess
import sys
import time


def deadline(_signum, _frame):
    raise TimeoutError

p = argparse.ArgumentParser()
p.add_argument('--labels', required=True)
p.add_argument('--export', required=True)
p.add_argument('--adapter', choices=('seosmith-v1', 'controlled-csv-v1'), required=True)
p.add_argument('--timeout', type=int, default=10, choices=range(1, 61))
p.add_argument('--memory-mib', type=int, default=1024, choices=range(64, 4097))
a = p.parse_args()

def limits():
    resource.setrlimit(resource.RLIMIT_AS, (a.memory_mib * 1024**2,) * 2)
    resource.setrlimit(resource.RLIMIT_CPU, (a.timeout, a.timeout + 1))

cmd = [sys.executable, str(Path(__file__).with_name('compare.py')), '--labels', a.labels, '--export', a.export, '--adapter', a.adapter]
start = time.monotonic()
with open(os.devnull, 'wb') as sink:
    proc = subprocess.Popen(cmd, stdout=sink, stderr=sink, start_new_session=True, preexec_fn=limits)
    signal.signal(signal.SIGALRM, deadline)
    signal.setitimer(signal.ITIMER_REAL, a.timeout)
    try:
        _, status, usage = os.wait4(proc.pid, 0)
        timed_out = False
    except TimeoutError:
        os.killpg(proc.pid, signal.SIGKILL)
        _, status, usage = os.wait4(proc.pid, 0)
        timed_out = True
    finally:
        signal.setitimer(signal.ITIMER_REAL, 0)
    proc.returncode = os.waitstatus_to_exitcode(status)
elapsed = time.monotonic() - start
print(json.dumps({'measurement_version': 1, 'scope': 'offline_comparison_process', 'elapsed_seconds': elapsed, 'peak_rss_kib_linux': usage.ru_maxrss, 'timeout_seconds': a.timeout, 'address_space_limit_mib': a.memory_mib, 'timed_out': timed_out, 'exit_code': proc.returncode, 'fetch_count': None, 'discovery_count': None, 'reason_unavailable': 'comparison reads retained files and does not instrument a crawl'}, indent=2))
sys.exit(0 if proc.returncode == 0 and not timed_out else 1)
