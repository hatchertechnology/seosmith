import { test } from 'node:test'
import assert from 'node:assert/strict'
import { readFile } from 'node:fs/promises'
import { createHash } from 'node:crypto'
import { PGlite } from '@electric-sql/pglite'
const hash = x => createHash('sha256').update(x).digest('hex')
test('Seosmith authenticated pushes, idempotency, revocation, rename isolation and ownership', async () => {
 const db = new PGlite()
 try {
  await db.exec(`create role anon; create role authenticated; create role service_role bypassrls;
   create schema auth; create table auth.users(id uuid primary key);
   create function auth.jwt() returns jsonb language sql stable as $$ select coalesce(nullif(current_setting('request.jwt.claims',true),''),'{}')::jsonb $$;
   create function auth.uid() returns uuid language sql stable as $$ select (auth.jwt()->>'sub')::uuid $$;
   grant usage on schema auth,public to authenticated,anon,service_role;`)
  await db.exec(await readFile(new URL('../migrations/20261028142440_create_websites.sql',import.meta.url),'utf8'))
  await db.exec(await readFile(new URL('../migrations/20261031042314_seosmith_audits.sql',import.meta.url),'utf8'))
  const owner='00000000-0000-0000-0000-000000000001', other='00000000-0000-0000-0000-000000000002', site='00000000-0000-0000-0000-000000000003', id='00000000-0000-0000-0000-000000000004', token='a'.repeat(32)
  await db.query('insert into auth.users values ($1),($2)',[owner,other])
  await db.query("insert into public.websites(id,owner_id,fqdn) values($1,$2,'example.com')",[site,owner])
  await db.query('insert into public.seosmith_bindings(website_id,token_sha256) values($1,$2)',[site,hash(token)])
  const r={schema_version:1,request:{request_id:id,website_id:site,binding_generation:1,url:'https://example.com/'},outcome:'completed',pages:[],findings:[]}
  const push=async (t=token,report=r,sha=null)=>{const body=JSON.stringify(report);return (await db.query('select public.ingest_seosmith_audit($1,$2,$3,$4) as receipt',[t,id,sha??hash(body),body])).rows[0].receipt}
  await db.exec('set role anon')
  await assert.rejects(()=>push('b'.repeat(32)),/invalid_worker/)
  await assert.rejects(()=>push(token,r,'0'.repeat(64)),/invalid_delivery/)
  await assert.rejects(()=>push(token,{...r,request:{...r.request,website_id:other}}),/invalid_binding/)
  assert.equal((await push()).accepted,true)
  assert.deepEqual(await push(),await push())
  await assert.rejects(()=>push(token,{...r,outcome:'partial'}),/delivery_conflict/)
  await assert.rejects(()=>db.query('select * from public.seosmith_audits'),/permission denied/)
  const identity=async (user,anonymous=false)=>{await db.exec('reset role;set role authenticated');await db.query("select set_config('request.jwt.claims',$1,false)",[JSON.stringify({sub:user,is_anonymous:anonymous})])}
  await identity(other);await assert.rejects(()=>db.query('select public.read_seosmith_audits($1)',[site]),/forbidden/)
  await identity(owner,true);await assert.rejects(()=>db.query('select public.read_seosmith_audits($1)',[site]),/forbidden/)
  await identity(owner);assert.equal((await db.query('select public.read_seosmith_audits($1) as audits',[site])).rows[0].audits.length,1)
  await db.exec('reset role');await db.query("update public.websites set fqdn='new.example.com' where id=$1",[site])
  await identity(owner);assert.equal((await db.query('select public.read_seosmith_audits($1) as audits',[site])).rows[0].audits.length,0)
  await db.exec('reset role');await db.query('update public.seosmith_bindings set enabled=false where website_id=$1',[site]);await db.exec('set role anon')
  await assert.rejects(()=>push(),/invalid_worker/)
 } finally {await db.close()}
})
