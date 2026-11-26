import json,hashlib,pathlib,copy
D=pathlib.Path('fixtures/contracts/s14-wire-1-candidate')
base=json.loads((D/'v4-candidate-complete.json').read_bytes())
def write(name,data):
 (D/name).write_bytes(data if isinstance(data,bytes) else json.dumps(data,ensure_ascii=False,separators=(',',':')).encode())
def variant(name,fn):
 p=copy.deepcopy(base);fn(p);write('invalid-'+name+'.json',p)
variant('unknown-wire',lambda p:p['policy_guidance'].__setitem__('version','unknown'))
variant('missing-manifest',lambda p:p['policy_guidance'].pop('manifest'))
variant('wrong-policy-digest',lambda p:p['policy_guidance']['candidate'].__setitem__('policy_digest','sha256:'+'0'*64))
variant('wrong-source',lambda p:p['policy_guidance']['candidate']['records'][0].__setitem__('source_locator','/pages/0/description'))
variant('forged-state',lambda p:p['policy_guidance']['candidate']['records'][0].__setitem__('outcome','no_violation'))
variant('unknown-config-key',lambda p:p['policy_guidance']['config'].__setitem__('extra',1))
variant('missing-coverage',lambda p:p['policy_guidance']['candidate']['coverage'].pop())
raw=(D/'v4-candidate-complete.json').read_bytes()
write('invalid-schema-fraction.json',raw.replace(b'"schema_version":4',b'"schema_version":4.0',1))
write('invalid-duplicate-root.json',raw.replace(b'"schema_version":4',b'"schema_version":4,"schema_version":4',1))
variant('config-boolean-string', lambda p:p['policy_guidance']['config'].__setitem__('forbid_redirects','false'))
variant('duplicate-manifest-row', lambda p:p['policy_guidance']['manifest'].__setitem__(1,p['policy_guidance']['manifest'][0]))
variant('negative-count', lambda p:p['policy_guidance']['candidate']['coverage'][0].__setitem__('candidates',-1))
write('invalid-schema-exponent.json',raw.replace(b'"schema_version":4',b'"schema_version":4e0',1))
write('invalid-escaped-duplicate-root.json',raw.replace(b'"schema_version":4',b'"schema_version":4,"schema_\\u0076ersion":4',1))
write('invalid-escaped-duplicate-nested.json',raw.replace(b'"policy_guidance":{"version":"s14-wire-1"', b'"policy_guidance":{"version":"s14-wire-1","v\\u0065rsion":"s14-wire-1"', 1))
p=copy.deepcopy(base);c=p['policy_guidance']['candidate'];policy=c['policy_canonical_json'];policy=policy.replace('{"catalog_version":', '{"catalog_version":"s14-1","catalog_version":', 1);digest='sha256:'+hashlib.sha256(policy.encode()).hexdigest();c['policy_canonical_json']=policy;c['policy_digest']=digest
for r in c['records']: r['policy_ref']=digest
write('invalid-policy-duplicate-key.json',p)
# invalid-omitted-forged-pass.json was derived from v4-candidate-omitted.json, which failed full-population admission; both were deleted (decision (c)).
registry={'status':'candidate-not-frozen','catalog':base['policy_guidance']['catalog'],'manifest_rules':[{k:v for k,v in x.items() if k!='candidates'} for x in base['policy_guidance']['manifest']]};write('registry-candidate.json',registry)
matrix=json.loads(pathlib.Path('fixtures/policies/state-matrix.json').read_bytes());matrix['version']='s14-wire-1';matrix['status']='candidate-not-frozen';write('state-matrix-candidate.json',matrix)
files=[]
for f in sorted(D.glob('*.json')):
 if f.name=='manifest.json':continue
 b=f.read_bytes();files.append({'path':f.name,'bytes':len(b),'sha256':hashlib.sha256(b).hexdigest(),'expected_report_validation':'reject' if f.name.startswith('invalid-') else ('accept' if f.name.startswith('v4-') else 'reference')})
(D/'manifest.json').write_text(json.dumps({'status':'candidate-not-frozen','note':'Report validation expectations exclude authentication and trusted ingestion scope; see ingestion-cases-candidate.json.','files':files},indent=2)+'\n')
