# HTP receiver integration

HTP's expanded receiver is implemented and deployed following
[HTP PR #4](https://github.com/hatchertechnology/htp/pull/4). Its current source and
migration history live in the HTP repository; see `docs/seosmith.md` there.

The existing `seosmith.rs`, SQL, test and wiring patch in this directory are the
**initial 0.1 bootstrap snapshot**, not an up-to-date deployment package. They lack
the later history, privileges, expanded evidence and policy-exclusion migrations.
Do not apply this bootstrap SQL over an existing HTP deployment or replace HTP's
current receiver with these files. Use HTP's own migration/deployment workflow.

## Verify current compatibility

With Node.js, Cargo and HTP's `supabase/` dependencies installed:

```sh
node integrations/htp/verify-contract.mjs /path/to/htp
```

This generates real reports through SEOsmith's fixture crawler and applies HTP's
merged audit migrations to isolated PGlite state. It verifies full/partial/robots
coverage, structured/social data, typed relationships, exact original bytes and
SHA-256 receipts, idempotent retries, conflicting deliveries, manual import and
owner isolation. It uses no live network endpoints, production database or real
credentials. Temporary fixture output is cleaned up after the run.

The current contract remains schema 1 with additive fields. HTP supports up to
20,000 pages plus 20,000 resources (40,000 records combined), 400,000 findings and
16 MiB exact report bytes. See [extraction contract](../../docs/EXTRACTION.md).

## Worker provisioning

The ingest route is separate from Supabase user JWT middleware. It forwards the
scoped worker credential only to `ingest_seosmith_audit`; that database function
checks its hash against a website binding and validates website ID, generation,
FQDN, report hash, schema, limits and idempotency. It returns a receipt only after
its transaction commits. No database service-role credential is placed on Thor.

Owner-authenticated history is available at
`GET /websites/{id}/seo/audits?limit=10`. To paginate, use both `before` (the last
scan_at) and `before_id` (the last delivery_id); neither alone is a cursor.

Provision `seosmith_bindings` using HTP's privileged database administration path,
with the existing website UUID, SHA-256 of a freshly generated secret, generation
and enabled flag. Store the raw secret only in SOPS/the Seosmith secret mount.
Rotate the token or disable the row to revoke ingestion. Increment generation
when crawl configuration or website bindings change. Never commit a real token.

The HTP application/UI must authorize scheduling separately; a Seosmith worker
credential is not a user login. Report reads preserve owner and current-FQDN
checks. UI implementation remains exclusively in HTP.
