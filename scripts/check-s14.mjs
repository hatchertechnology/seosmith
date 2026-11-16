import { readFileSync } from 'node:fs'
import { createHash } from 'node:crypto'
import assert from 'node:assert/strict'

function validateRaw(raw) {
  let i = 0
  const ws = () => { while (/\s/.test(raw[i] ?? '') && i < raw.length) i++ }
  function string() {
    const start = i++
    while (i < raw.length) {
      if (raw[i] === '\\') { i += 2; continue }
      if (raw[i++] === '"') return JSON.parse(raw.slice(start, i))
    }
    throw Error('unterminated string')
  }
  function value(depth) {
    ws()
    if (raw[i] === '"') { string(); return }
    if (raw[i] === '{' || raw[i] === '[') {
      if (++depth > 64) throw Error('depth')
      const object = raw[i++] === '{', close = object ? '}' : ']'
      const keys = new Set()
      ws()
      if (raw[i] === close) { i++; return }
      while (true) {
        ws()
        if (object) {
          if (raw[i] !== '"') throw Error('key')
          const key = string()
          if (keys.has(key)) throw Error('duplicate')
          keys.add(key); ws()
          if (raw[i++] !== ':') throw Error('colon')
        }
        value(depth); ws()
        if (raw[i] === close) { i++; return }
        if (raw[i++] !== ',') throw Error('comma')
      }
    }
    const start = i
    while (i < raw.length && !/[\s,}\]]/.test(raw[i])) i++
    JSON.parse(raw.slice(start, i))
  }
  value(0); ws()
  if (i !== raw.length) throw Error('trailing content')
}

const sha = s => 'sha256:' + createHash('sha256').update(s).digest('hex')
const json = x => JSON.stringify(x)
const fail = (test, message) => { if (!test) throw Error(message) }
function fields(o,required,optional=[]) { fail(o && typeof o==='object' && !Array.isArray(o) && required.every(k=>Object.hasOwn(o,k)) && Object.keys(o).every(k=>required.includes(k)||optional.includes(k)),'closed fields') }
const nbytes = s => Buffer.byteLength(s, 'utf8')
const tagged = fields => fields.map(([k,v]) => `${k}=${nbytes(v)}:${v}\n`).join('')
export const scopeHash = r => sha('seosmith-scope-v1\n' + tagged([['website_id',r.website_id],['binding_generation',String(r.binding_generation)]]))
export function manifestHash(rows) {
  const defs=rows.map(r=>[r.category,r.rule_id,r.rule_version,r.expected_pointer]).sort((a,b)=>Buffer.compare(Buffer.from(a.slice(0,3).join('\0')),Buffer.from(b.slice(0,3).join('\0'))))
  let data=`seosmith-manifest-v1\nrows=${defs.length}\n`
  for (const [category,rule,version,expected] of defs) data+=tagged([['category',category],['rule_id',rule],['rule_version',version],['expected_type',expected===null?'null':'pointer'],['expected',expected??'']])
  return sha(data+'end\n')
}
function canonical(x) {
  if (typeof x==='number') { fail(Number.isSafeInteger(x),'unsafe number'); return json(x) }
  if (typeof x==='string') { fail(nbytes(x)<=16384 && !/[\uD800-\uDBFF](?![\uDC00-\uDFFF])|(?<![\uD800-\uDBFF])[\uDC00-\uDFFF]/u.test(x),'string'); return json(x) }
  if (Array.isArray(x)) return '['+x.map(canonical).join(',')+']'
  if (x && typeof x==='object') return '{'+Object.keys(x).sort((a,b)=>Buffer.compare(Buffer.from(a),Buffer.from(b))).map(k=>canonical(k)+':'+canonical(x[k])).join(',')+'}'
  return json(x)
}
const defs = [
  ['advisory','short_title','title_min','title'],['advisory','long_title','title_max','title'],['advisory','short_description','description_min','description'],['advisory','long_description','description_max','description'],['advisory','little_content','words_min','words'],['advisory','too_many_links','links_max','links'],['observation','noindex',null,'robots'],['observation','http_redirect',null,'status'],['observation','alternates_observed',null,'alternates'],['observation','external_target',null,'status'],['policy_violation','noindex','require_indexable','robots'],['policy_violation','http_redirect','forbid_redirects','status'],['observation','external_link',null,'links']
]
const matrix=JSON.parse(readFileSync(new URL('../fixtures/policies/state-matrix.json',import.meta.url))).tuples
function pointer(obj,p) { fail(typeof p==='string' && nbytes(p)<=1024 && /^(\/(?:[^~]|~[01])*)+$/.test(p),'pointer'); let value=obj; for(const part of p.slice(1).split('/')) { const k=part.replace(/~1/g,'/').replace(/~0/g,'~'); fail(value!==null && typeof value==='object' && Object.hasOwn(value,k),'missing pointer'); value=value[k] } return value }
function entity(page) { return sha(`seosmith-entity-v1\n${page.is_resource?'resource':'page'}\n${page.url}`) }
export function canonicalSeed(value) {
  try { const u=new URL(value); return typeof value==='string' && ['http:','https:'].includes(u.protocol) && !!u.hostname && !u.username && !u.password && !u.port && !u.search && !u.hash && u.href===value; } catch { return false; }
}
export function validateS14(raw) {
  fail(nbytes(raw)<=16*1024*1024,'report limit'); validateRaw(raw); const report=JSON.parse(raw)
  const stringsBounded = v => typeof v==='string' ? nbytes(v)<=16384 : Array.isArray(v) ? v.every(stringsBounded) : v && typeof v==='object' ? Object.entries(v).every(([k,x])=>nbytes(k)<=16384 && stringsBounded(x)) : true;
  fail(stringsBounded(report),'report string limit');
  fail(report.schema_version===3 && report.policy_guidance?.candidate && Array.isArray(report.policy_guidance.manifest),'shape')
  const g=report.policy_guidance,c=g.candidate,m=g.manifest,p=JSON.parse(c.policy_canonical_json)
  fields(c,['policy_canonical_json','policy_digest','scope_digest','manifest_digest','observed_at','records','coverage']);
  fields(c.observed_at,['started_at','finished_at']);
  const policyKeys=['description_max','description_min','forbid_redirects','links_max','require_indexable','title_max','title_min','words_min'];
  fail(json(Object.keys(g.config).sort())===json(policyKeys),'policy fields');
  for(const key of policyKeys) fail(['forbid_redirects','require_indexable'].includes(key)?typeof g.config[key]==='boolean':Number.isSafeInteger(g.config[key]) && g.config[key]>=0 && g.config[key]<=1000000,'policy types');
  fail(g.config.title_min<=g.config.title_max && g.config.description_min<=g.config.description_max,'policy ranges');
  validateRaw(c.policy_canonical_json); fail(nbytes(c.policy_canonical_json)<=65536 && canonical(p)===c.policy_canonical_json && sha(c.policy_canonical_json)===c.policy_digest,'policy canonical/digest')
  fail(typeof report.coverage_complete==='boolean','coverage_complete type');
  const safeUint = n => Number.isSafeInteger(n) && n>=0;
  const absoluteHttp = value => { try { const u=new URL(value); return typeof value==='string' && ['http:','https:'].includes(u.protocol) && !!u.hostname && !u.username && !u.password } catch { return false } };
  fail(Array.isArray(report.pages),'page array');
  for(const page of report.pages) {
    fail(absoluteHttp(page.url) && typeof page.is_resource==='boolean' && typeof page.blocked_by_robots==='boolean' && typeof page.extraction_truncated==='boolean' && (page.error===null || typeof page.error==='string') && (page.status===null || (safeUint(page.status) && page.status<=65535)) && safeUint(page.words) && typeof page.content_type==='string' && typeof page.title==='string' && typeof page.description==='string' && typeof page.robots==='string' && Array.isArray(page.links) && Array.isArray(page.alternates), 'page field types');
    for(const link of page.links) fail(link && absoluteHttp(link.url),'invalid link URL');
    for(const alternate of page.alternates) fail(alternate && typeof alternate.language==='string' && absoluteHttp(alternate.url),'invalid alternate');
  }
  const req=report.request; fail(/^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/.test(req.website_id) && Number.isSafeInteger(req.binding_generation) && req.binding_generation>=0,'scope fields');
  fail(canonicalSeed(req.url) && /^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/.test(req.request_id) && typeof req.check_external_links==='boolean' && ['max_pages','max_resources','max_duration_seconds','delay_ms'].every(k=>safeUint(req[k])) && req.max_pages>=1 && req.max_pages<=20000 && req.max_resources<=20000 && req.max_duration_seconds>=10 && req.max_duration_seconds<=7200 && req.delay_ms>=1000 && req.delay_ms<=60000,'request field types or canonical seed');
  const expected={catalog_version:'s14-1',collection:{check_external_links:req.check_external_links,max_pages:req.max_pages,max_resources:req.max_resources,seed_url:req.url},policy:g.config,schema_version:3,validator_version:'s14-1'}
  fail(canonical(expected)===c.policy_canonical_json,'policy/request')
  fail(c.scope_digest===scopeHash(req) && c.manifest_digest===manifestHash(m),'scope/manifest digest')
  fail(json(c.observed_at)===json({started_at:report.started_at,finished_at:report.finished_at}),'interval')
  fail(m.length===defs.length && nbytes(json(m))<=262144 && nbytes(json(c.coverage))<=262144 && c.records.length<=512 && c.records.filter(r=>r.category==='observation').length<=128,'bounds')
  const rows=new Map(); for(const d of defs) rows.set(d[0]+'\n'+d[1],d)
  fail(Array.isArray(report.pages) && Array.isArray(c.records) && Array.isArray(c.coverage),'arrays');
  const seenRows=new Set(); for(const row of m) { fields(row,['category','rule_id','rule_version','expected_pointer','candidates']); const key=row.category+'\n'+row.rule_id,d=rows.get(key); fail(d && !seenRows.has(key) && row.rule_version==='s14-1' && row.expected_pointer===(d[2]?'/policy/'+d[2]:null),'manifest rule'); seenRows.add(key); fail(Array.isArray(row.candidates) && new Set(row.candidates).size===row.candidates.length,'manifest candidates') }
  const inScope = value => { try { const u=new URL(value), seed=new URL(req.url); return ['http:','https:'].includes(u.protocol) && u.hostname===seed.hostname && !u.port && !u.username && !u.password } catch { return false } };
  for(const row of m) {
    const candidates=[];
    for(const page of report.pages) {
      const id=entity(page), inside=inScope(page.url);
      if(row.rule_id==='external_link') { if(!page.is_resource && inside) page.links.forEach((link,i)=>{if(!inScope(link.url)) candidates.push(id+'|edge:'+i)}); }
      else if(row.category!=='observation' || (row.rule_id==='external_target' ? !inside : inside && !page.is_resource)) candidates.push(id)
    }
    fail(json(row.candidates)===json(candidates),'candidate universe');
  }
  fail(c.coverage.length===m.length,'coverage count'); const ids=new Set()
  for (const r of c.records) {
    fields(r,['record_id','category','rule_id','rule_version','entity_id','discriminator','eligibility','attempt','outcome','actual','source_locator','expected','confidence','interval_ref','policy_ref','reason'],['observed_fact','context_locator','target_locator']);
    fail([r.category,r.rule_id,r.rule_version,r.entity_id,r.discriminator].every(s=>typeof s==='string' && s.length>0 && nbytes(s)<=256 && /^[\x20-\x7e]+$/.test(s)),'identity components');
    const row=m.find(x=>x.category===r.category && x.rule_id===r.rule_id), d=rows.get(r.category+'\n'+r.rule_id); fail(row && d && r.rule_version===row.rule_version,'record rule')
    fail(r.record_id===sha(`seosmith-record-v1\n${r.category}\n${r.rule_id}\n${r.rule_version}\n${r.entity_id}\n${r.discriminator}`) && !ids.has(r.record_id),'record id'); ids.add(r.record_id)
    fail(row.candidates.includes(r.discriminator==='self'?r.entity_id:r.entity_id+'|'+r.discriminator),'record scope')
    const source=report.pages.find(page=>entity(page)===r.entity_id); fail(source,'entity missing');
    if(r.discriminator==='self') {
      const mime=source.content_type.split(';')[0].trim().toLowerCase();
      const html=source.status>=200 && source.status<300 && ['text/html','application/xhtml+xml','application/vnd.wap.xhtml+xml'].includes(mime);
      const unavailable=source.blocked_by_robots || !!source.error || source.status===null || source.extraction_truncated || (r.category==='observation' && ['noindex','alternates_observed'].includes(r.rule_id) && !html);
      const applicable=inScope(source.url) && !source.is_resource && (r.rule_id==='http_redirect'||html) && (r.rule_id!=='noindex'||p.policy.require_indexable) && (r.rule_id!=='http_redirect'||p.policy.forbid_redirects) && (!['short_title','long_title'].includes(r.rule_id)||source.title.length>0) && (!['short_description','long_description'].includes(r.rule_id)||source.description.length>0);
      const expectedEligibility=unavailable?'unknown':r.category==='observation'?'eligible':applicable?'eligible':'ineligible';
      fail(r.eligibility===expectedEligibility,'eligibility evidence');
    }
    if(r.discriminator!=='self') {
      const n=Number(r.discriminator.slice(5)); fail(r.discriminator===`edge:${n}` && Number.isSafeInteger(n) && n>=0 && source.links[n],'edge ordinal');
      const target=report.pages.find(page=>page.url===source.links[n].url);
      const available=!source.extraction_truncated&&!source.blocked_by_robots&&!source.error && target && target.status!==null && !target.error && !target.blocked_by_robots;
      fail(r.outcome===(available?'observed':'unknown'),'edge availability');
      if(!available) { const reason=source.extraction_truncated||source.blocked_by_robots||source.error?'source_unavailable':target?'external_target_unavailable':req.check_external_links?'external_target_not_fetched':'external_check_disabled'; fail(r.reason===reason,'edge reason') }
    }
    const tuple=matrix.find(t=>['category','eligibility','attempt','outcome','actual'].every(k=>t[k]===r[k])); fail(tuple && tuple.confidence===r.confidence && r.interval_ref==='candidate.observed_at' && r.policy_ref===c.policy_digest,'tuple/inheritance')
    fail(tuple.reason==='required'?typeof r.reason==='string' && nbytes(r.reason)>0 && nbytes(r.reason)<=256 && r.source_locator===null:r.reason===null && typeof r.source_locator==='string','reason/source')
    if(tuple.expected==='policy_source') { fail(r.expected===row.expected_pointer,'expected rule'); pointer(p,r.expected) } else fail(r.expected===null,'expected null')
    fail((r.category==='observation' && r.attempt==='completed')===Object.hasOwn(r,'observed_fact'),'observation fact presence');
    if(r.discriminator!=='self') fail(typeof r.context_locator==='string','edge context required');
    else fail(r.context_locator==null && r.target_locator==null,'self provenance forbidden');
    const loc=r.source_locator??r.context_locator; if(loc) { const match=/^\/pages\/(0|[1-9]\d*)\/(.+)$/.exec(loc); fail(match,'source'); const page=report.pages[Number(match[1])]; fail(page && entity(page)===r.entity_id,'source entity'); pointer(report,loc); if(r.discriminator==='self') { fail(match[2]===d[3],'source field');
    if(r.attempt==='completed' && r.category!=='observation') {
      const threshold=p.policy[d[2]]; let triggered;
      if(r.rule_id==='short_title' || r.rule_id==='short_description') triggered=Array.from(page[d[3]]).length<threshold;
      else if(r.rule_id==='long_title' || r.rule_id==='long_description') triggered=Array.from(page[d[3]]).length>threshold;
      else if(r.rule_id==='little_content') triggered=page.words<threshold;
      else if(r.rule_id==='too_many_links') triggered=page.links.length>threshold;
      else if(r.rule_id==='noindex') {fail(threshold===true,'noindex intent'); triggered=page.robots.split(/[, ;]/).some(v=>['noindex','none'].includes(v.toLowerCase()))}
      else if(r.rule_id==='http_redirect') {fail(threshold===true,'redirect intent'); triggered=page.status>=300 && page.status<400}
      fail(r.outcome===(r.category==='advisory'?(triggered?'advisory_triggered':'advisory_not_triggered'):(triggered?'violation':'no_violation')),'outcome evidence');
    }
    if(r.category==='observation') { let fact; if(r.rule_id==='noindex') fact=page.robots.split(/[, ;]/).some(v=>['noindex','none'].includes(v.toLowerCase())); else if(r.rule_id==='http_redirect') fact=page.status>=300 && page.status<400; else if(r.rule_id==='alternates_observed') fact={count:page.alternates.length,x_default_present:page.alternates.some(a=>a.language.toLowerCase()==='x-default')}; else fact=page.status; fail(json(r.observed_fact)===json(fact),'observation fact') } } else { fail(r.rule_id==='external_link' && match[2]===`links/${r.discriminator.slice(5)}` && r.context_locator===loc,'edge binding'); if(r.outcome==='observed') { const status=pointer(report,r.target_locator), link=pointer(report,r.context_locator), target=/^\/pages\/(0|[1-9]\d*)\/status$/.exec(r.target_locator); fail(target && report.pages[Number(target[1])].url===link.url && status!==null && !report.pages[Number(target[1])].error && !report.pages[Number(target[1])].blocked_by_robots && r.observed_fact===status,'target binding') } else fail(r.target_locator==null,'unknown target') } }
  }
  const covKeys=new Set(); for(const cRow of c.coverage) { fields(cRow,['category','rule_id','candidates','emitted','omitted','omission_reason','complete','eligibility_counts','attempt_counts','outcomes','unknown_reasons']); const key=cRow.category+'\n'+cRow.rule_id,row=m.find(r=>r.category+'\n'+r.rule_id===key); fail(row && !covKeys.has(key) && cRow.candidates===row.candidates.length,'coverage row'); covKeys.add(key); const emitted=c.records.filter(r=>r.category===cRow.category && r.rule_id===cRow.rule_id); fail(cRow.emitted===emitted.length && cRow.omitted===cRow.candidates-cRow.emitted,'omitted'); fail(cRow.omitted===0?cRow.omission_reason===null:['record_limit','observation_limit'].includes(cRow.omission_reason),'omission reason'); for(const [field,rec] of [['eligibility_counts','eligibility'],['attempt_counts','attempt'],['outcomes','outcome']]) { const counts=cRow[field]; fail(Object.values(counts).every(v=>Number.isSafeInteger(v)&&v>=0) && Object.values(counts).reduce((a,b)=>a+b,0)===cRow.candidates,'coverage sums'); for(const state of new Set(emitted.map(r=>r[rec]))) fail((counts[state]??0)>=emitted.filter(r=>r[rec]===state).length,'coverage emitted') } const E=cRow.eligibility_counts,A=cRow.attempt_counts,O=cRow.outcomes;
    fail(Object.keys(E).every(k=>['eligible','ineligible','unknown'].includes(k)) && Object.keys(A).every(k=>['completed','not_attempted','unknown'].includes(k)),'coverage states');
    const allowed=matrix.filter(t=>t.category===cRow.category).map(t=>t.outcome);
    fail(Object.keys(O).every(k=>allowed.includes(k)),'outcome states');
    const eligible=(E.eligible??0), ineligible=(E.ineligible??0), unknown=(E.unknown??0);
    fail((A.completed??0)===eligible && (A.not_attempted??0)===ineligible+unknown && (A.unknown??0)===0 && (O.unknown??0)===unknown && (O.not_applicable??0)===ineligible && Object.entries(O).filter(([k])=>!['unknown','not_applicable'].includes(k)).reduce((n,[,v])=>n+v,0)===eligible,'coverage tuple reconciliation');
    fail(Object.keys(cRow.unknown_reasons).length<=64 && Object.keys(cRow.unknown_reasons).every(k=>nbytes(k)>0 && nbytes(k)<=256) && Object.values(cRow.unknown_reasons).every(v=>Number.isSafeInteger(v)&&v>0) && Object.values(cRow.unknown_reasons).reduce((a,b)=>a+b,0)===(cRow.outcomes.unknown??0),'unknown sum');
    for(const reason of new Set(emitted.filter(r=>r.outcome==='unknown').map(r=>r.reason))) fail((cRow.unknown_reasons[reason]??0)>=emitted.filter(r=>r.outcome==='unknown'&&r.reason===reason).length,'unknown emitted reason'); fail(!cRow.complete || (report.coverage_complete && cRow.omitted===0 && !(cRow.outcomes.unknown>0) && (!['external_link','external_target'].includes(cRow.rule_id) || req.check_external_links) && (cRow.rule_id!=='external_link' || report.pages.filter(p=>!p.is_resource&&inScope(p.url)).every(p=>!p.extraction_truncated&&!p.blocked_by_robots&&!p.error&&p.status!==null))),'complete') }
  return true
}
export function resolvesS14(previousRaw,currentRaw,recordId) {
  try {
    validateS14(previousRaw); validateS14(currentRaw);
    const old=JSON.parse(previousRaw), now=JSON.parse(currentRaw), a=old.policy_guidance.candidate, b=now.policy_guidance.candidate;
    const previous=a.records.find(r=>r.record_id===recordId), current=b.records.find(r=>r.record_id===recordId);
    const coverage=b.coverage.find(row=>row.category===current?.category && row.rule_id===current?.rule_id);
    return Boolean(previous && current && coverage?.complete && now.coverage_complete && a.scope_digest===b.scope_digest && a.policy_digest===b.policy_digest && a.manifest_digest===b.manifest_digest && previous.category==='policy_violation' && previous.outcome==='violation' && previous.discriminator==='self' && current.discriminator==='self' && current.category==='policy_violation' && current.eligibility==='eligible' && current.attempt==='completed' && current.outcome==='no_violation');
  } catch { return false; }
}
if (process.argv[2]) { validateS14(readFileSync(process.argv[2],'utf8')); console.log('S14 local contract accepted') }
