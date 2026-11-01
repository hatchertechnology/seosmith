// Local-only producer/receiver compatibility check. No live endpoints or credentials.
import assert from 'node:assert/strict'
import { createHash, randomUUID } from 'node:crypto'
import { readFile, mkdtemp, rm } from 'node:fs/promises'
import { createRequire } from 'node:module'
import { tmpdir } from 'node:os'
import { resolve, join } from 'node:path'
import { fileURLToPath } from 'node:url'
import { spawnSync } from 'node:child_process'

const htp = process.argv[2]
if (!htp) throw new Error('Usage: node integrations/htp/verify-contract.mjs /path/to/htp')
const root = fileURLToPath(new URL('../../', import.meta.url))
const receiver = resolve(htp)
const require = createRequire(join(receiver, 'supabase/package.json'))
const { PGlite } = require('@electric-sql/pglite')
const digest = value => createHash('sha256').update(value).digest('hex')
const output = await mkdtemp(join(tmpdir(), 'seosmith-contract-'))
let db
try {
  const fixtures = spawnSync('cargo', [
    'test', '--locked', '--test', 'extraction', 'export_htp_contract_reports', '--', '--ignored', '--exact',
  ], { cwd: root, env: { ...process.env, SEOSMITH_CONTRACT_DIR: output }, stdio: 'inherit' })
  if (fixtures.error) throw fixtures.error
  assert.equal(fixtures.status, 0, 'collector fixture generation must pass')
  db = new PGlite()
  await db.exec(`create role anon; create role authenticated; create role service_role bypassrls;
    create schema auth; create table auth.users(id uuid primary key);
    create function auth.jwt() returns jsonb language sql stable as $$select coalesce(nullif(current_setting('request.jwt.claims',true),''),'{}')::jsonb$$;
    create function auth.uid() returns uuid language sql stable as $$select (auth.jwt()->>'sub')::uuid$$;
    grant usage on schema auth,public to authenticated,anon,service_role;
    alter default privileges grant execute on functions to anon,authenticated;`)
  for (const migration of [
    '20261028142440_create_websites.sql',
    '20261031053344_seosmith_audits.sql',
    '20261031053413_seosmith_history.sql',
    '20261031053524_seosmith_explicit_function_privileges.sql',
    '20261101054016_seosmith_expanded_evidence.sql',
    '20261101060006_seosmith_policy_exclusions.sql',
  ]) await db.exec(await readFile(join(receiver, 'supabase/migrations', migration), 'utf8'))
  const other = randomUUID()
  await db.query('insert into auth.users values($1)', [other])
  for (const scenario of ['complete', 'resource_limited', 'robots_excluded']) {
    const bytes = await readFile(join(output, `${scenario}.json`), 'utf8')
    const report = JSON.parse(bytes), id = report.request.request_id, site = report.request.website_id
    const token = `local-fixture-only-${randomUUID()}`
    await db.exec('reset role')
    const owner = randomUUID()
    await db.query('insert into auth.users values($1)', [owner])
    await db.query("insert into public.websites(id,owner_id,fqdn) values($1,$2,'example.com')", [site, owner])
    await db.query('insert into public.seosmith_bindings(website_id,token_sha256,generation) values($1,$2,$3)', [site, digest(token), report.request.binding_generation])
    await db.exec('set role anon')
    const push = (body = bytes, hash = digest(body)) => db.query(
      'select public.ingest_seosmith_audit($1,$2,$3,$4) as receipt', [token, id, hash, body],
    ).then(result => result.rows[0].receipt)
    const receipt = await push()
    assert.deepEqual(receipt, { accepted: true, delivery_id: id, sha256: digest(bytes) })
    assert.deepEqual(await push(), receipt, 'retry retains the same delivery identity')
    await assert.rejects(() => push(bytes, '0'.repeat(64)), /invalid_delivery/)
    await assert.rejects(() => push(`${bytes} `), /delivery_conflict/)
    await db.exec('reset role; set role authenticated')
    await db.query("select set_config('request.jwt.claims',$1,false)", [JSON.stringify({ sub: owner, is_anonymous: false })])
    const saved = (await db.query('select public.seosmith_audit_detail($1,$2) as detail', [site, id])).rows[0].detail
    assert.deepEqual(saved.report, report, 'no producer observations may be discarded')
    assert.equal(saved.report_json, bytes, 'original UTF-8 bytes must survive')
    assert.equal(saved.summary.summary_version, 3)
    assert.equal(saved.summary.pages.checked, report.pages.filter(page => !page.is_resource).length)
    assert.equal(saved.summary.resources.checked, report.pages.filter(page => page.is_resource).length)
    assert.equal(saved.summary.coverage_complete, scenario === 'complete')
    const home = saved.report.pages.find(page => page.url === 'https://example.com/')
    assert.deepEqual(home.open_graph['og:image'], ['/one.webp', '/two.webp'])
    assert.deepEqual(home.twitter['twitter:card'], ['summary'])
    assert(home.structured_data.some(block => block.format === 'json_ld' && block.data['@type'] === 'WebPage'))
    const microdata = home.structured_data.find(block => block.format === 'microdata')
    assert.equal(microdata.data.properties.address[0].properties.streetAddress[0], '123 Main')
    assert(home.relationships.some(relation => relation.kind === 'navigation' && relation.descriptor === 'sponsored' && relation.state === 'not_fetched'))
    if (scenario === 'complete') {
      assert(saved.report.findings.some(finding => finding.rule === 'broken_resource' && finding.evidence_items.length && finding.details.observation_count))
    }
    if (scenario === 'resource_limited') assert.equal(saved.summary.stop_reason, 'resource_limit')
    assert.deepEqual((await db.query('select public.import_seosmith_audit($1,$2) as receipt', [site, bytes])).rows[0].receipt, receipt)
    await db.query("select set_config('request.jwt.claims',$1,false)", [JSON.stringify({ sub: other, is_anonymous: false })])
    await assert.rejects(() => db.query('select public.seosmith_audit_detail($1,$2)', [site, id]), /forbidden/)
    console.log(`Verified ${scenario}: exact report round-trip, receipt/retry, summaries, metadata and owner isolation`)
  }
} catch (error) {
  console.error(`Contract verification failed: ${error.message}`)
  process.exitCode = 1
} finally {
  if (db) await db.close()
  await rm(output, { recursive: true, force: true })
}
