#!/usr/bin/env python3
"""Regenerate provisional review bytes, not a delivery implementation."""
import hashlib
import json
import pathlib
import subprocess

root = pathlib.Path(__file__).resolve().parent.parent
out = root / 'fixtures/contracts/s14-wire-1-candidate'
out.mkdir(parents=True, exist_ok=True)

def dump(value):
    return json.dumps(value, ensure_ascii=False, separators=(',', ':')).encode()

def convert(p):
    r = p['request']; g = p['policy_guidance']; c = g['candidate']
    p['schema_version'] = 4; g['version'] = 's14-wire-1'
    policy = {'catalog_version':'s14-1', 'collection':{'check_external_links':r['check_external_links'], 'delay_ms':r['delay_ms'], 'max_duration_seconds':r['max_duration_seconds'], 'max_pages':r['max_pages'], 'max_resources':r['max_resources'], 'seed_url':r['url']}, 'engine_version':p['engine_version'], 'policy':g['config'], 'ruleset_version':p['ruleset_version'], 'schema_version':4, 'validator_version':'s14-wire-1', 'wire_contract_version':'s14-wire-1'}
    canonical = json.dumps(policy, ensure_ascii=False, sort_keys=True, separators=(',', ':'))
    digest = 'sha256:' + hashlib.sha256(canonical.encode()).hexdigest()
    c['policy_canonical_json'] = canonical; c['policy_digest'] = digest
    for record in c['records']: record['policy_ref'] = digest
    return p

for source in sorted((root / 'fixtures/policies').glob('v3-local-*.json')):
    (out / source.name.replace('v3-local-', 'v4-candidate-')).write_bytes(dump(convert(json.loads(source.read_bytes()))))
mixed = subprocess.check_output(['cargo', 'run', '--quiet', '--example', 'make_mixed_review'], cwd=root)
(out / 'v4-candidate-mixed-omitted.json').write_bytes(dump(convert(json.loads(mixed))))
for case in ('control', 'source_missing_status', 'source_non_html', 'source_non_success', 'source_error', 'source_blocked', 'source_truncated', 'target_missing_status', 'target_error', 'target_blocked', 'omitted'):
    output = subprocess.check_output(['cargo', 'run', '--quiet', '--features', 'policy-wire-prototype', '--example', 'make_wire_edge_review', '--', case], cwd=root)
    (out / f'v4-candidate-edge-{case}.json').write_bytes(output)
ambiguous = subprocess.check_output(['cargo', 'run', '--quiet', '--features', 'policy-wire-prototype', '--example', 'make_wire_edge_review', '--', 'ambiguous'], cwd=root)
(out / 'invalid-ambiguous-external-target.json').write_bytes(dump(convert(json.loads(ambiguous))))

for case in ('short', 'equal'):
    local = subprocess.check_output(['cargo', 'run', '--quiet', '--example', 'make_unicode_review', '--', case], cwd=root)
    (out / f'v4-candidate-unicode-{case}.json').write_bytes(dump(convert(json.loads(local))))
raw = (out / 'v4-candidate-complete.json').read_bytes(); cap = 4952756
(out / 'v4-candidate-whitespace.json').write_bytes(raw + b'\n')
(out / 'v4-candidate-exact-cap.json').write_bytes(raw + b' ' * (cap - len(raw)))
(out / 'invalid-cap-plus-one.json').write_bytes(raw + b' ' * (cap + 1 - len(raw)))
for depth in (64, 65):
    p = json.loads(raw); value = 0
    for _ in range(depth - 4): value = [value]
    p['findings'][0]['details']['depth_probe'] = value
    (out / ('v4-candidate-depth64.json' if depth == 64 else 'invalid-depth65.json')).write_bytes(dump(p))
for generation in (0, 2):
    p = json.loads(raw); r = p['request']; r['binding_generation'] = generation
    def tag(k, v):
        v = str(v); return f'{k}={len(v.encode())}:{v}\n'
    scope = 'seosmith-scope-v1\n' + tag('website_id', r['website_id']) + tag('binding_generation', generation)
    p['policy_guidance']['candidate']['scope_digest'] = 'sha256:' + hashlib.sha256(scope.encode()).hexdigest()
    (out / f'v4-candidate-generation-{generation}.json').write_bytes(dump(p))
# Raw embedded-policy scanner boundary vectors; not valid closed policy objects.
for depth in (64, 65):
    value = 0
    for _ in range(depth - 1): value = [value]
    (out / f'policy-raw-depth-{depth}.json').write_bytes(dump({'probe':value}))
for size in (65536, 65537):
    empty = dump({'probe':''})
    (out / f'policy-raw-bytes-{size}.json').write_bytes(dump({'probe':'a' * (size - len(empty))}))
(out / 'policy-raw-escaped-duplicate.json').write_bytes(b'{"x":1,"\\u0078":2}')
p = json.loads(raw); p['findings'][0]['details']['claimed_ingestion'] = 'worker'
(out / 'v4-candidate-forged-source-claim.json').write_bytes(dump(p))
subprocess.run(['python3', str(root / 'scripts/make-s14-review-schema.py')], cwd=root, check=True)
subprocess.run(['python3', str(root / 'scripts/make-s14-review-negatives.py')], cwd=root, check=True)
