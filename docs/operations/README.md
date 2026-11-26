# Local operations for Codex, Claude and Pi

This is the **new** Wave 0 adapter, not a claim that older installations have
these commands. The existing `crawl`, `rules`, `serve` and private HTTP API remain
available. No HTP account, credentials, network access to HTP, browser worker,
public endpoint, or MCP server is needed for local operations.

## Discover before using

Resolve an explicitly configured executable to a trusted absolute regular,
executable file. Do not search an untrusted working directory or accept a shell
command string as a binary. Invoke argv arrays with shell execution disabled:

```python
from pathlib import Path
import os, subprocess
binary = Path(configured_binary).resolve(strict=True)
if not binary.is_file() or not os.access(binary, os.X_OK):
    raise RuntimeError("missing_binary")
result = subprocess.run([str(binary), "operations", "capabilities"],
                        capture_output=True, timeout=5, check=True,
                        env={"PATH": "/usr/bin:/bin"})
```

The executable must be operator-installed/trusted; `is_file` is not a signature
check. Verify `adapter_version == 1` and the requested capability ID. Missing
binary, unknown command, timeout, bad JSON or incompatible version means
unavailable/unsupported; never substitute `crawl` for an absent status operation.
`--version` alone is insufficient (this additive adapter retains package 0.2.0).
Capabilities do not access the database or network. Version fields describe this
compiled producer, not every retained report. The global `--evidence-contract-v2` flag or `SEOSMITH_EVIDENCE_CONTRACT_V2=true` is rejected for operations, including capabilities. Legacy crawl and serve use the S01 evidence-v2 gate; operations remain v1-only. Advanced evidence-v2, authorized
staging profiles (S13) and baseline CI (S16) are advertised false. Future support
must be explicitly detected after those contracts ship; do not invent flags.

## Explicit start

Use an absolute database filename **per intended audit** in a directory accessible
only to the operator. Create the directory with mode 0700 (or equivalent ACL).
New database, lock and export files use mode 0600 on Unix. Retained reports may
contain customer evidence: filesystem access is the local authorization boundary.
Do not put credentials in seed URLs, request files, argv, logs or evidence. This
adapter neither reads nor forwards API/HTP credential environment variables.

The request shape is the existing `CrawlRequest`. Every execution bound must be
explicit; defaults accepted by the older CLI are deliberately rejected here:

```json
{
  "request_id": "b2a844c8-5bc9-49e5-bcf0-8fd1407d69f4",
  "website_id": "4822f25e-af3c-4c60-a8c6-31bd99d2acda",
  "binding_generation": 1,
  "url": "https://example.com/",
  "max_pages": 20,
  "max_resources": 20,
  "max_duration_seconds": 60,
  "delay_ms": 1000,
  "check_external_links": false
}
```

For standalone work, `website_id` is an operator-assigned local identity and
`binding_generation` is local metadata. Neither grants HTP access or proves
customer ownership. Both UUIDs must be non-nil. Core validation rejects unknown
fields, seed credentials/query/fragment, non-HTTP(S) and custom ports. Scope is
exact host (including subdomain), not a path prefix. The production crawler
retains public-IP/DNS pinning, robots, redirect, response and extraction limits.

| Bound | Accepted range |
| --- | --- |
| Request JSON | at most 16 KiB |
| Pages | 1–20,000 |
| Resources | 0–20,000 |
| Crawl duration | 10–7,200 seconds |
| Per-origin delay | 1,000–60,000 ms |
| Retained/exported report | at most 16 MiB |

```sh
/opt/seosmith --database /private/audit-uuid/run.sqlite operations start --request /private/request.json
```

Start is synchronous. A host may supervise this command using its background
execution facility; it must not invent a detached server or shell wrapper.
The request UUID is known before launch. Poll status from a separate invocation.
No other job is drained. The database has an adapter marker, and existing service
or legacy crawl databases are rejected. There is no implicit job recovery.

Identical retries (same UUID and parsed payload) return existing status, including
while a worker owns the lock. Changed payload with the same UUID is a conflict;
a new UUID in the same database is also a conflict. A new intended audit requires
both a fresh UUID and fresh database. Retries never re-crawl a completed, failed,
cancelled or interrupted run. A crash between database creation and marker creation
fails closed; retain that directory for inspection and use a fresh audit identity.

## Status, cancellation and failure

```sh
/opt/seosmith --database /private/audit-uuid/run.sqlite operations status --id b2a844c8-5bc9-49e5-bcf0-8fd1407d69f4
/opt/seosmith --database /private/audit-uuid/run.sqlite operations cancel --id b2a844c8-5bc9-49e5-bcf0-8fd1407d69f4
```

Status includes `request_id`, `state`, `execution_state`, `worker_active`,
`report_available`, `delivery_state` and sanitized `error`. Core states are
queued/running/completed/partial/failed/cancelled. `execution_state=unknown` means
a queued/running record has no worker lock owner (for example a killed process).
Status never resumes work. `worker_active` is null if the lock file is missing; no worker absence or terminal cancellation is inferred. `worker_active` is an advisory lock observation, not
proof of progress. These reads do not create databases or run migrations.

Cancel includes `cancel_accepted` and `cancellation_terminal`. Accepted means the
queued/running state was changed, not that the worker has stopped. A worker may
still be finishing an in-flight fetch or sitemap discovery. Only report terminal
cancellation once `cancellation_terminal=true`. Duplicate/terminal cancellation
returns `cancel_accepted=false`; completed reports are not changed. A race between
cancellation and finish is serialized; cancellation that wins prevents a clean
completion report. Cancellation without a report retains checkpoints, not a
fabricated clean audit.

The adapter adds a hard timeout around the entire crawler future, including
initial discovery. Deadline exhaustion records `failed` with
`adapter_deadline_exceeded`; internal crawl failure records `crawl_internal_error`;
a report over the byte limit records `report_byte_limit`.
Checkpoints remain, but these failures may have no final report.

`jobs.error` takes one of a fixed set of codes (`src/generation_error.rs`,
`JobErrorCode`). Hosted `serve` jobs can also record the schema-4 generation codes:

| Code | Meaning |
|---|---|
| `crawl_internal_error` | The crawl future returned an error. Also the fallback of `Store::fail`. |
| `report_byte_limit` | The report is over a byte bound (16 MiB report, or the 4,952,756 B HTP cap). |
| `policy_manifest_limit` | The S14 manifest is over 262,144 B (it grows with crawled entities and with out-of-scope links). |
| `evidence_extension_limit` | `evidence_contract` is over its 1 MiB extension limit. |
| `policy_extension_limit` | `policy_guidance` or its canonical policy is over its limit. |
| `population_mismatch` | A recomputed population (findings, edges, candidate contract) differs from the retained one. |
| `generation_invariant` | Any other generation check, including an unclassified generation error. |
| `store_error` | Persisting the finished report failed. |
| `adapter_deadline_exceeded` | The operations adapter ran out of time. |

The worker logs the code, and for generation failures a fixed detail, never the
error chain, a request body or a token. Codes are only ever added; the stored text
is a contract with the status API. A host kill can
leave execution unknown. Never interpret zero findings, missing report, partial
coverage, a process exit, or accepted cancellation as a clean audit.

A completed operation returns exit 0 and JSON even when the crawl outcome is
failed/partial: inspect the state and report coverage. Invocation/storage failures
return exit 1 and a compact JSON stderr error; there is no traceback/error chain.
Clap syntax errors return exit 2. Read/status/export can be retried. For uncertain
start delivery, retry only the exact original identity and payload. No automated
new-UUID retry. Poll at a bounded interval/deadline chosen by the host.

## Exact report and export

```sh
/opt/seosmith --database /private/audit-uuid/run.sqlite operations report --id b2a844c8-5bc9-49e5-bcf0-8fd1407d69f4
/opt/seosmith --database /private/audit-uuid/run.sqlite operations export --id b2a844c8-5bc9-49e5-bcf0-8fd1407d69f4 --output /private/report.json
```

Report writes only original retained UTF-8 JSON bytes to stdout, with no added
newline or reserialization. Export writes the same bytes to a **new** file and
returns request ID, byte count and SHA-256; it rejects existing paths/symlinks.
A failed disk write may leave a partial export file; it is not a successful export
without the receipt. Use a new destination for retry and verify the receipt hash.
Report size is checked in SQLite before loading the text. Unavailable/expired and
oversized reports fail explicitly. Keep original `schema_version`, engine/ruleset
versions, `outcome`, `stop_reason`, `coverage_complete`, and any future evidence
contract fields; unknown schema/evaluation states must remain unknown.

Local retention is operator-managed; no pruning or HTP delivery worker runs here.
`delivery_state=disabled` is independent of crawl completion. Legacy `crawl` and `serve` reject adapter-owned databases before recovery; do not use them to attempt delivery. Optional HTP delivery requires an
HTP-authorized orchestration path and separate scoped ingestion credentials.
Delivery failure must remain distinct from local crawl completion.

## Existing private HTTP operations

Direct private-worker access is an operator-only alternative, not this local
adapter's transport. Configure the endpoint explicitly on the protected network;
keep existing service authentication/network safeguards. Do not make it public.
Existing routes are POST `/v1/runs`, GET `/v1/runs/{id}`, POST
`/v1/runs/{id}/cancel`, GET `/v1/runs/{id}/report`, and GET `/v1/rules`.
There is no remote export/capabilities endpoint. HTTP export means saving original
report response bytes locally with a bounded client, not calling an invented API.

Use the separately provisioned worker bearer token from an operator secret store
or `SEOSMITH_API_TOKEN_FILE`, never argv or request JSON. Disable redirects/proxy
inheritance and use TLS except on an explicitly trusted private tunnel. The token
is instance/operator scoped, not customer OAuth. HTP must establish customer
ownership; a caller-supplied website ID is never authorization. 401 is auth failure,
409 on submit is conflict/full queue (do not guess which), 404 report is unavailable,
503/connection failure is worker/storage unavailable. Preserve uncertainty after
a timed-out mutation; reconcile by immutable ID. Existing `serve` still requires
HTP delivery configuration; this adapter does not change that service contract.
