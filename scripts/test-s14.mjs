import { readFileSync } from 'node:fs'
import { createHash } from 'node:crypto'
import assert from 'node:assert/strict'
import { validateS14, scopeHash, manifestHash, resolvesS14, canonicalSeed } from './check-s14.mjs'
const read = p => readFileSync(new URL('../fixtures/policies/'+p,import.meta.url),'utf8')
const raw=read('v3-local-complete.json'), base=JSON.parse(raw)
for (const vector of JSON.parse(read('seed-url-vectors.json'))) assert.equal(canonicalSeed(vector.value),vector.accepted,vector.value)
for(const name of ['v3-local-complete.json','v3-local-partial.json','v3-local-omitted.json','v3-local-intent.json','v3-local-blocked.json','v3-local-reordered.json','v3-local-resolved.json']) assert.equal(validateS14(read(name)),true,name)
const intent=JSON.parse(read('v3-local-intent.json'));
assert.ok(intent.policy_guidance.candidate.records.some(r=>r.rule_id==='noindex'&&r.category==='policy_violation'&&r.outcome==='violation'));
assert.ok(intent.policy_guidance.candidate.records.some(r=>r.rule_id==='http_redirect'&&r.category==='policy_violation'&&r.outcome==='violation'));
const noindex=intent.policy_guidance.candidate.records.find(r=>r.category==='policy_violation'&&r.rule_id==='noindex'&&r.outcome==='violation');
assert.equal(resolvesS14(read('v3-local-intent.json'),read('v3-local-resolved.json'),noindex.record_id),true);
assert.equal(resolvesS14(read('v3-local-intent.json'),read('v3-local-partial.json'),noindex.record_id),false);
const missingDigest=JSON.parse(read('v3-local-resolved.json')); delete missingDigest.policy_guidance.candidate.scope_digest;
assert.equal(resolvesS14(read('v3-local-intent.json'),JSON.stringify(missingDigest),noindex.record_id),false);
const missingCoverage=JSON.parse(read('v3-local-resolved.json')); missingCoverage.policy_guidance.candidate.coverage=[];
assert.equal(resolvesS14(read('v3-local-intent.json'),JSON.stringify(missingCoverage),noindex.record_id),false);
const blocked=JSON.parse(read('v3-local-blocked.json'));
assert.ok(blocked.policy_guidance.candidate.records.some(r=>r.rule_id==='external_link'&&r.outcome==='unknown'));
assert.ok(!blocked.findings.some(f=>f.url==='https://outside.example/available'&&f.rule==='robots_blocked'));
const reordered=JSON.parse(read('v3-local-reordered.json'));
assert.notEqual(reordered.pages[0].links[0].url,base.pages[0].links[0].url);
const oldEdge=base.policy_guidance.candidate.records.find(r=>r.rule_id==='external_link'&&r.discriminator==='edge:0');
assert.equal(resolvesS14(raw,read('v3-local-reordered.json'),oldEdge.record_id),false);

const identity=JSON.parse(read('identity-vectors.json')), request=base.request, manifest=base.policy_guidance.manifest;
assert.equal(scopeHash(request), identity.scope.digest);
assert.equal(scopeHash({...request,request_id:'00000000-0000-4000-8000-000000000999',delay_ms:2000}), identity.scope.digest);
assert.equal(scopeHash({...request,website_id:'00000000-0000-4000-8000-000000000999'}), identity.scope.different_website);
assert.equal(scopeHash({...request,binding_generation:request.binding_generation+1}), identity.scope.different_binding);
assert.equal(manifestHash(manifest), identity.manifest.digest);
let changed=structuredClone(manifest); changed[0].candidates=[]; assert.equal(manifestHash(changed),identity.manifest.different_population); assert.equal(identity.manifest.digest,identity.manifest.different_population);
changed=structuredClone(manifest); changed[0].rule_version='s14-2'; assert.equal(manifestHash(changed),identity.manifest.different_version);
changed=structuredClone(manifest); changed[0].expected_pointer='/policy/title_max'; assert.equal(manifestHash(changed),identity.manifest.different_expected);
assert.equal(manifestHash(manifest.slice(0,-1)),identity.manifest.removed_rule);
changed=structuredClone(manifest); changed.push({category:'observation',rule_id:'added',rule_version:'s14-1',expected_pointer:null,candidates:[]}); assert.equal(manifestHash(changed),identity.manifest.added_rule);
const vectors=JSON.parse(read('policy-identity-vectors.json'))
for(const v of vectors) assert.equal('sha256:'+createHash('sha256').update(v.canonical_utf8).digest('hex'),v.sha256,v.name)
assert.equal(vectors[0].sha256,vectors[1].sha256)
for (const vector of vectors.slice(0,2)) {
  const req={...base.request,...vector.request_delta};
  const policy=JSON.parse(vector.canonical_utf8);
  assert.deepEqual(policy.collection,{check_external_links:req.check_external_links,max_pages:req.max_pages,max_resources:req.max_resources,seed_url:req.url});
}
for(const v of vectors.slice(2)) assert.notEqual(v.sha256,vectors[0].sha256,v.name)
const mutations={
 tuple:r=>r.policy_guidance.candidate.records[0].outcome='violation',
 missing_manifest:r=>r.policy_guidance.manifest.pop(),
 wrong_policy_pointer:r=>r.policy_guidance.candidate.records[0].expected='/policy/title_max',
 wrong_source:r=>r.policy_guidance.candidate.records[0].source_locator='/pages/1/title',
 digest:r=>r.policy_guidance.candidate.policy_digest='sha256:'+'0'.repeat(64),
 scope:r=>r.policy_guidance.candidate.scope_digest='sha256:'+'0'.repeat(64),
 manifest_digest:r=>r.policy_guidance.candidate.manifest_digest='sha256:'+'0'.repeat(64),
 coverage_tuple:r=>{let c=r.policy_guidance.candidate.coverage[0];c.eligibility_counts={unknown:c.candidates}},
 omitted:r=>r.policy_guidance.candidate.coverage[0].omitted++,
 duplicate_record:r=>r.policy_guidance.candidate.records.push(structuredClone(r.policy_guidance.candidate.records[0])),
 missing_coverage:r=>r.policy_guidance.candidate.coverage.pop(),
 unsafe_integer:r=>r.policy_guidance.config.title_min=9007199254740992,
 policy_limit:r=>r.policy_guidance.candidate.policy_canonical_json='x'.repeat(65537),
 schema1:r=>r.schema_version=1,
 schema2:r=>r.schema_version=2,
 unknown_self_target:r=>{const a=r.policy_guidance.candidate.records.find(x=>x.discriminator==='self'&&x.outcome==='unknown'); if(a) a.target_locator='/pages/0/status'; else {const x=r.policy_guidance.candidate.records[0];x.target_locator='/pages/0/status'}},
 coverage_string:r=>r.coverage_complete='false',
 external_string:r=>r.request.check_external_links='true',
 status_string:r=>r.pages[0].status='200',
 words_string:r=>r.pages[0].words='0',
 pages_string:r=>r.request.max_pages='500',
 self_target:r=>r.policy_guidance.candidate.records[0].target_locator='/pages/0/status',
 self_context:r=>r.policy_guidance.candidate.records[0].context_locator='/pages/0/title',
 bad_external_url:r=>r.pages[0].links[0].url='not a url',
 negative_reason:r=>r.policy_guidance.candidate.coverage[0].unknown_reasons={a:-1,b:1},
 zero_reason:r=>r.policy_guidance.candidate.coverage[0].unknown_reasons={extra:0},
 seed_uppercase:r=>r.request.url='https://EXAMPLE.com/',
 seed_no_slash:r=>r.request.url='https://example.com',
 uuid_uppercase:r=>r.request.website_id='AAAAAAAA-AAAA-4AAA-8AAA-AAAAAAAAAAAA',

}
for(const t of JSON.parse(read('negative-cases.json'))) {
 let text
 if(t.mutation==='duplicate_key') text=raw.replace('{"schema_version":3','{"schema_version":3,"schema_version":3')
 else if(t.mutation==='depth_65') text=raw.replace(/}$/,',"deep":'+ '['.repeat(65)+'0'+']'.repeat(65)+'}')
 else {const r=structuredClone(base);mutations[t.mutation](r);
   if(['external_string','pages_string','seed_uppercase','seed_no_slash'].includes(t.mutation)) {
     const c=r.policy_guidance.candidate,p=JSON.parse(c.policy_canonical_json);
     p.collection.seed_url=r.request.url; p.collection.max_pages=r.request.max_pages; p.collection.check_external_links=r.request.check_external_links;
     c.policy_canonical_json=JSON.stringify(p); c.policy_digest='sha256:'+createHash('sha256').update(c.policy_canonical_json).digest('hex');
     for(const record of c.records) record.policy_ref=c.policy_digest;
   }
   text=JSON.stringify(r)}
 assert.throws(()=>validateS14(text),undefined,t.name)
}
const malformedResolution=JSON.parse(read('v3-local-resolved.json')); malformedResolution.coverage_complete='false';
assert.equal(resolvesS14(read('v3-local-intent.json'),JSON.stringify(malformedResolution),noindex.record_id),false);
console.log('Validated 7 positive reports, '+vectors.length+' policy vectors, and '+JSON.parse(read('negative-cases.json')).length+' negative cases')
