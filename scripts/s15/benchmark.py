#!/usr/bin/env python3
"""Run the controlled SEOsmith engine executable with process and I/O bounds."""
import argparse
import hashlib
import json
import os
from pathlib import Path
import resource
import signal
import sqlite3
import subprocess
import tempfile
import time

p = argparse.ArgumentParser()
p.add_argument('--binary', required=True, help='absolute path to compiled s15_controlled example')
p.add_argument('--timeout', type=int, default=15, choices=range(5, 61))
p.add_argument('--memory-mib', type=int, default=1024, choices=range(128, 4097))
p.add_argument('--report', help='new file for reserialized JSON report, for comparison')
p.add_argument('--mode', choices=('budget', 'labeled'), default='budget')
a = p.parse_args()
binary = Path(a.binary)
if not binary.is_absolute() or not binary.is_file() or not os.access(binary, os.X_OK):
    p.error('binary must be an absolute path to an executable file')

def limits():
    resource.setrlimit(resource.RLIMIT_AS, (a.memory_mib * 1024**2,) * 2)
    resource.setrlimit(resource.RLIMIT_FSIZE, (16 * 1024**2,) * 2)
    resource.setrlimit(resource.RLIMIT_CPU, (a.timeout, a.timeout + 1))

def deadline(_signal, _frame):
    raise TimeoutError

with tempfile.TemporaryDirectory(prefix='s15-benchmark-') as directory:
    db = Path(directory) / 'crawl.sqlite'
    output = Path(directory) / 'output.json'
    start = time.monotonic()
    with output.open('wb') as out, open(os.devnull, 'wb') as err:
        process = subprocess.Popen([str(binary), str(db), a.mode], stdout=out, stderr=err, start_new_session=True, preexec_fn=limits)
        signal.signal(signal.SIGALRM, deadline)
        signal.setitimer(signal.ITIMER_REAL, a.timeout)
        try:
            _, status, usage = os.wait4(process.pid, 0)
            timed_out = False
        except TimeoutError:
            os.killpg(process.pid, signal.SIGKILL)
            _, status, usage = os.wait4(process.pid, 0)
            timed_out = True
        finally:
            signal.setitimer(signal.ITIMER_REAL, 0)
        process.returncode = os.waitstatus_to_exitcode(status)
    elapsed = time.monotonic() - start
    if process.returncode != 0 or timed_out:
        raise SystemExit(f'benchmark failed: exit={process.returncode}, timed_out={timed_out}')
    result = json.loads(output.read_bytes())
    report = result['report']
    with sqlite3.connect(f'file:{db}?mode=ro', uri=True) as conn:
        frontier = conn.execute('SELECT url, page IS NOT NULL, in_sitemap FROM frontier ORDER BY url').fetchall()
    report_bytes = json.dumps(report, separators=(',', ':'), ensure_ascii=False).encode()
    if a.report:
        with open(a.report, 'xb') as f:
            f.write(report_bytes)
    print(json.dumps({'benchmark_version': 1, 'scope': 'controlled_transport_crawler_engine', 'binary_sha256': hashlib.sha256(binary.read_bytes()).hexdigest(), 'mode': a.mode, 'politeness_delay_ms': 1000, 'elapsed_seconds': elapsed, 'cpu_seconds': usage.ru_utime + usage.ru_stime, 'peak_rss_kib_linux': usage.ru_maxrss, 'timeout_seconds': a.timeout, 'address_space_limit_mib': a.memory_mib, 'fetch_attempts': result['fetch_attempts'], 'fetch_attempt_count': len(result['fetch_attempts']), 'frontier_unique_including_seed': len(frontier), 'frontier': [{'url': u, 'observed': bool(done), 'in_sitemap': bool(sitemap)} for u, done, sitemap in frontier], 'completed_observations': len(report['pages']), 'outcome': report['outcome'], 'stop_reason': report['stop_reason'], 'coverage_complete': report['coverage_complete'], 'report_sha256': hashlib.sha256(report_bytes).hexdigest()}, indent=2))
