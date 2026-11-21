#!/usr/bin/env python3
"""Check local normalization evidence; this does not approve a wire URL grammar."""
import json
import pathlib
import subprocess

root = pathlib.Path(__file__).resolve().parent.parent
fixtures = root / 'fixtures/contracts/s14-wire-1-candidate'

def produce(mode):
    return subprocess.check_output(['cargo', 'run', '--quiet', '-j', '1', '--features', 'policy-wire-prototype', '--example', 'make_wire_url_review', '--', mode], cwd=root)

matrix_bytes = produce('matrix')
report_bytes = produce('report')
assert matrix_bytes == (fixtures / 'url-normalization-cases-candidate.json').read_bytes()
assert report_bytes == (fixtures / 'v4-candidate-url-normalization.json').read_bytes()
matrix = json.loads(matrix_bytes)['cases']
report = json.loads(report_bytes)
kept = [case for case in matrix if case['normalized'] is not None]
assert [link['url'] for link in report['pages'][0]['links']] == [case['normalized'] for case in kept]
assert len(matrix) == 12 and len(kept) == 10
edges = [r for r in report['policy_guidance']['candidate']['records'] if r['rule_id'] == 'external_link']
assert len(edges) == sum(not case['in_scope'] for case in kept) == 6
assert all(r['outcome'] == 'unknown' and r['reason'] == 'external_target_not_fetched' for r in edges)
assert kept[6]['normalized'] == 'http://[::ffff:c000:201]/path'
print('12 URL normalization cases, 10 stored occurrences and 6 external unknown edges verified')
