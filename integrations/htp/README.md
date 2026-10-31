# HTP receiver integration

These files are mirrored from the accompanying implementation in the local HTP
checkout. The endpoint and database migration were tested locally; no production
migration, credential provisioning or deployment has been performed.

- `seosmith.rs` belongs at `apps/api/src/seosmith.rs` in HTP.
- SQL belongs in `supabase/migrations/` with its generated migration name.
- `seosmith.test.mjs` belongs in `supabase/tests/` and uses the project's PGlite tests.
- Add `WebsiteStore::ingest_seosmith` and merge the routers as in `htp-wiring.patch`.

The ingest route is separate from Supabase user JWT middleware. It forwards the
scoped worker credential only to `ingest_seosmith_audit`; that database function
checks its hash against a website binding and validates website ID, generation,
FQDN, report hash, schema, limits and idempotency. It returns a receipt only after
its transaction commits. No database service-role credential is placed on Thor.

Owner-authenticated history is available at
`GET /websites/{id}/seo/audits?limit=10`. To paginate, use both `before` (the last
received_at) and `before_id` (the last delivery_id); neither alone is a cursor.

Provision `seosmith_bindings` using HTP's privileged database administration path,
with the existing website UUID, SHA-256 of a freshly generated secret, generation
and enabled flag. Store the raw secret only in SOPS/the Seosmith secret mount.
Rotate the token or disable the row to revoke ingestion. Increment generation
when crawl configuration or website bindings change. Never commit a real token.

The HTP application/UI must authorize scheduling separately; a Seosmith worker
credential is not a user login. Report reads preserve owner and current-FQDN
checks. UI implementation remains exclusively in HTP.
