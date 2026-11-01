# Seosmith

Rust technical SEO auditing backend for HTP. No frontend, MySQL, browser runtime,
paid API, or SaaS subscription is required. A single binary provides a durable
crawl queue, HTTP API, audit rules, and an outbound HTP delivery worker. HTP owns
user authentication, website ownership, the UI, and long-term report history.

## Implemented

- HTTP(S) crawling with exact-host scope, fragment normalization, URL deduplication,
  robots.txt, sitemap indexes, namespaced/gzipped sitemaps and optional external
  link observations. Redirect targets are queued explicitly; automatic HTTP
  redirects are disabled.
- HTML metadata, headings, links, images, canonical/hreflang relationships,
  indexability directives, status/redirect problems, duplicated content/metadata,
  sitemap inconsistencies, forms, basic security headers and performance hints.
- Canonical/hreflang target discovery, meta-refresh redirects, responsive images,
  scripts/styles/media/preloads and recursive CSS dependency observations, including
  public CDN resources. Separate resource budgets and typed relationship coverage.
- Bounded JSON-LD and nested microdata extraction, repeated Open Graph/Twitter
  metadata, declared character encodings, header hreflang/canonical fallback,
  BCP47 validation, and multi-target finding evidence. See [extraction contract](docs/EXTRACTION.md).
  `seosmith rules` emits the authoritative versioned rule catalog.
- SQLite/WAL jobs, frontier checkpoints, exclusive process lock, cancellation,
  restart recovery, immutable reports and idempotent job submission.
- **Push into HTP:** persist report before sending, reuse delivery IDs on retries,
  exponential backoff, exact SHA-256 receipts, and retain unacknowledged results.
- Private authenticated API, credential files, per-request DNS checking and IP
  pinning, private/metadata/transition-network denial, response size limits,
  per-origin request delays and explicit partial-coverage reporting.

This is an HTP-oriented implementation of SEOnaut's audit concepts, not a claim
of byte-for-byte behavioral parity. See [coverage and limits](docs/COVERAGE.md)
and [upstream attribution](docs/ATTRIBUTION.md).

## Build and verify

Rust 1.98.1 (edition 2024), a C compiler and normal Cargo build prerequisites:

```sh
cargo fmt --all --check
cargo clippy --locked --all-targets -- -D warnings
cargo test --locked --all-targets
cargo build --locked --release
```

Tests use deterministic fixture websites and local HTTP receivers; they do not
crawl a real client's website or deliver to production HTP. The library exposes
an injectable `Transport` for crawler tests; the production transport always
checks public addresses.

## One-off audit

Create `request.json` (use your actual HTP website UUID):

```json
{
  "request_id": "b2a844c8-5bc9-49e5-bcf0-8fd1407d69f4",
  "website_id": "4822f25e-af3c-4c60-a8c6-31bd99d2acda",
  "binding_generation": 1,
  "url": "https://example.com/",
  "max_pages": 500,
  "max_resources": 500,
  "max_duration_seconds": 1800,
  "delay_ms": 1000,
  "check_external_links": false
}
```

```sh
seosmith --database data/manual.sqlite crawl --request request.json > report.json
```

Use a fresh request UUID for each intended audit. Reusing the UUID with different
configuration fails. The CLI prints a report and persists it; automatic delivery
runs in `serve` mode. Use a dedicated database for isolated CLI audits.

## Service configuration

```text
SEOSMITH_DB=/data/seosmith.sqlite
SEOSMITH_BIND=0.0.0.0:8080
SEOSMITH_API_TOKEN_FILE=/run/secrets/api-token
HTP_INGEST_URL=https://htp-api-kerry-hatcher.fly.dev/integrations/seosmith/audits
HTP_INGEST_TOKEN_FILE=/run/secrets/htp-token
```

Tokens must have at least 32 characters. `_FILE` takes precedence over the same
variable without `_FILE`. Never commit these files. The API credential is distinct
from the HTP ingestion credential. HTTPS is mandatory for the configured HTP
endpoint. HTTP proxy environment variables and delivery redirects are ignored.

```sh
seosmith serve
```

One service instance currently has one HTP ingestion credential, provisioned for
one HTP website. Use separate instances/state volumes for independently scoped
website credentials. The API credential is for a trusted backend/operator, not
HTP end users. HTP must authorize any requested website before submitting a job.

## HTTP API

All `/v1` endpoints require `Authorization: Bearer <SEOSMITH_API_TOKEN>`.
Requests and reports are JSON; protected responses have `Cache-Control: no-store`.

| Method | Route | Behavior |
| --- | --- | --- |
| POST | `/v1/runs` | Submit the CrawlRequest above; 202 also for an identical retry; 409 for conflict/full queue |
| GET | `/v1/runs/{request_id}` | Crawl state, delivery state, attempts and sanitized error |
| GET | `/v1/runs/{request_id}/report` | Immutable complete report, or 404 when not available |
| POST | `/v1/runs/{request_id}/cancel` | Cancel queued/running work; completed runs are not cancelled |
| GET | `/v1/rules` | Rule IDs, descriptions and severities |
| GET | `/health/live` | Process liveness, no credential |
| GET | `/health/ready` | SQLite readiness, no credential |

Only one crawl runs at a time, while the delivery worker runs independently. Job
submission is limited to 100 queued/running/unacknowledged-report jobs. Page limits
are 1..20,000; a separate `max_resources` budget (0..20,000, default 500) covers
assets and external link observations. Resources do not consume the page budget. Deadlines 10..7200
seconds, and request delays 1000..60000ms. Current deadlines include downtime
between restarts. Fetching is HTTP only; scripts and form actions are never run.

## HTP push protocol

HTP implements `POST /integrations/seosmith/audits`; expanded evidence support was
merged in [HTP PR #4](https://github.com/hatchertechnology/htp/pull/4) and deployed.
HTP owns receiver source/migrations. The [integration notes](integrations/htp)
identify the older bootstrap snapshot and explain how to verify real collector
reports against the current HTP receiver without production access.

Each push uses the scoped HTP bearer credential and `Idempotency-Key: <request_id>`:

```json
{
  "delivery_id": "b2a844c8-5bc9-49e5-bcf0-8fd1407d69f4",
  "sha256": "<lowercase SHA-256 of the exact UTF-8 report_json bytes>",
  "report_json": "<serialized Report JSON string>"
}
```

The report includes schema/engine/ruleset versions, original request, timestamps,
terminal outcome, stop reason, coverage flag, page metadata and stable findings.
JSON is carried as a string so hashing is independent of the receiver's JSON
serializer. HTP acknowledges only after durable persistence:

```json
{
  "delivery_id": "b2a844c8-5bc9-49e5-bcf0-8fd1407d69f4",
  "sha256": "<same digest>",
  "accepted": true
}
```

Any HTTP error, timeout, redirect, oversized receipt, incorrect ID/digest or
`accepted: false` leaves the report pending. Backoff grows from 30 seconds to one
hour. A failed delivery never triggers another crawl. Reports are limited to
16 MiB; oversized results stay checkpointed and require a smaller audit scope.
Acknowledged local details expire after 30 days; compact idempotency records stay.
Pending delivery data is never automatically expired. Monitor the volume quota.

Seosmith does not open an HTP connection into Thor or expose a public crawl API.
Scheduling can initially submit jobs through the private API. An HTP scheduler
can be added later without changing the push contract; this release has no
built-in recurring scheduler or outbound job-claim protocol.

## Thor deployment

[deploy/kubernetes.yaml](deploy/kubernetes.yaml) contains a one-replica Recreate
Deployment, retained-state PVC, private ClusterIP Service and network policy.
It is a deployment example, not an applied or fully provisioned installation.

Before applying through Argo:

1. Build/publish the image and pin its verified digest in the manifest.
2. Confirm Thor capacity; create a dedicated ZFS dataset and static 10 GiB Retain
   PV named `seosmith-state`, node affinity to Thor and ownership for UID 10001.
3. Create `seosmith-credentials` through SOPS with keys `api-token` and `htp-token`.
4. Deploy/provision the HTP receiver and matching website/generation/token hash.
5. Verify DNS selectors, CNI enforcement and required egress. The example permits
   public IPv4 HTTP(S); IPv6-only sites need a reviewed matching egress policy.
6. Expose only a private Tailscale/operator route. Label only its authorized
   namespace `seosmith-access=true` after checking namespace ownership.
7. Back up SQLite consistently using its backup API or stop the service for a
   coordinated database/WAL copy; do not copy the main SQLite file alone live.
8. Test interrupted crawl recovery, HTP outage recovery, restore, and negative
   private-network fetches before scheduling real client sites.

A retained volume is not a backup. This single-node design is not highly available.
