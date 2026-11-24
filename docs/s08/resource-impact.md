# Saved local resource delivery impact

`seosmith --database PATH resource-impact REQUEST_ID` reads a terminal local-only
job's existing checkpoints through normal CLI dispatch and prints bounded JSON. It
joins the version 1 local `resource` measurements from [PR43](https://github.com/hatchertechnology/seosmith/pull/43)
with the version 1 `resource_references` observations from [PR52](https://github.com/hatchertechnology/seosmith/pull/52),
the saved page status and the saved static resource relationships.

This command is a local analysis surface, not a wire producer, report field,
delivery contract or S08 completion claim. It makes no requests, creates no
database, runs no migration or recovery, writes no file and never queues a
delivery. Rollback is to stop running the command; stored reports, outbox rows
and local checkpoint bytes are untouched.

## Prerequisites and snapshot safety

Use an offline, quiesced database snapshot. Close all users first, checkpoint or
remove the WAL, and do not start another writer while analysis runs. Immutable
SQLite bypasses normal locks, so it cannot detect a writer that starts after the
checks. The command refuses an existing `-wal` or `-journal` sidecar and refuses a
non-existent database. It requires a job in `local_only` state with
`execution_mode='local'`; standard, queued, running, failed and ambiguous jobs are
rejected.

A job without a `local_finalization` column is accepted as a legacy schema. Its
completion is reported as `unknown`; the command does not infer normal completion
from terminal state. `interrupted` means recovery closed the local job.

## Joined evidence and semantics

For each frontier row with `resource=1` the command reads the saved `resource`
collector. Only already-requested resources are considered. It does not fetch a
missing resource or add a resource to the frontier. HTML reference sources are
read from `resource_references`; the saved page relationship list supplies
crawler-observed dependency edges, including CSS `url()` dependencies. Reference
URLs are resolved against the saved source page URL. A `<base>` override is not
retained, so its effect is unknown.

Every metric carries explicit semantics:

| Metric | Semantics | Source |
| --- | --- | --- |
| `decoded_body` | `measured` decoded response body bytes | transport body already read by the local crawl |
| `declared_content_length` | `estimated` header value for a potentially encoded representation | `content-length` header |
| `transfer` | `unknown` | local checkpoint does not measure network transfer size |
| `body_sha256` | `measured_decoded_body` | SHA-256 of the decoded body |
| declared MIME | `declared_header` | `content-type` header, not proof of the bytes |
| detected signature/dimensions | `measured_byte_prefix` | supported PNG/GIF/JPEG/WebP prefix only |
| `content_encoding` | observed `declared_header`, absent means `unknown` | header; absence is not proof of identity encoding |
| transport decoding | `unknown` | transport decoding is not retained |
| `cache_control`, `etag`, `last_modified` | observed `declared_header`, absent means `unknown` | declared policy and validator fields, not proof of cache or revalidation behavior |

A missing or unavailable measurement is `null` with an `unknown` semantics and a
reason. Missing resource bytes are never written as zero. Aggregated
`decoded_body_bytes` is `null` when nothing was measured; otherwise it sums only
counted measurements and reports the number of resources whose decoded bytes are
unknown. A group that has no counted resources also has `null`, not zero.

Because a sum covers counted measurements only, a non-`null` total or group value
is a lower bound on the bytes actually delivered, never an exact total. It is
exact only when the same aggregate reports `complete=true`.

`complete` has the same meaning for the aggregate and for each origin/MIME group:
it is `false` for unknown measurements, partial collectors, excluded resources,
input truncation or a row limit. A group that retains an excluded member is
therefore incomplete even when every other member was measured, and
`excluded_resources` above zero always implies `complete=false` at both levels.

Totals count only observed resource measurements on a successful 2xx response.
Non-2xx responses, robots-blocked requests, fetch failures, unfetched resources,
page-evidence omissions and unavailable resource collectors remain visible in
the per-resource output with an `exclusion_reason`, but are excluded from totals.
A 404 body can therefore have measured bytes while its `counted_in_byte_totals`
is false.

Duplicate groups use equality of measured decoded-body hashes only. Equality
does not establish interchangeability. A resource with unknown bytes is not
placed in a duplicate group. Groups cover every resource with a measured body,
including excluded ones, so a non-2xx response can share a group with counted
resources; each member's `counted_in_byte_totals` and `exclusion_reason` say
whether its bytes are in the totals.

## Referring-page provenance and affected pages

Each resource lists observed referrers with the source page, relationship kind
and descriptor, or the retained static HTML element, attribute, raw value,
`srcset` candidate descriptor, media/type/sizes/loading/priority attributes.
Relationship and static-reference occurrences are kept separately. The static
collector does not retain the attribute name, so `link` elements report `href`
and every other element reports `src`; that derivation is not an independent
observation. Reference
coverage is `partial` when a relevant source is partial, unavailable or absent,
or when input is truncated; `unknown` when no relevant reference source was
retained; otherwise `bounded`. Even `bounded` means only the retained checkpoint,
never proof that every referring page was crawled or that every reference was
captured.

`affected_pages` is the static dependency-to-page graph computed by PR43's
`affected_pages` over saved resource relationships, including propagation through
a CSS resource to its resource dependencies. It is not a browser loading graph.
Affected-page coverage is `partial` when a page JSON is omitted, extraction was
truncated or input was truncated, `unknown` when no retained HTML source page is
available, and otherwise `bounded`. `discovery_coverage` is always `unknown`:
terminal local state cannot prove discovery completeness. Static observations cannot establish
rendered oversizing, the actual LCP element, transfer performance or field data.

## Bounds

The read is streamed. It retains at most 1,000 frontier rows, 64 KiB of evidence
per row, 2 MiB of aggregate evidence, 512 KiB of page JSON per row and 8 MiB of
aggregate page JSON. Output is capped at 4 MiB and fails without partial JSON
when exceeded. At most 5,000 referrers (100 per resource), 5,000 affected-page
links (500 per resource) and 1,000 omission records are retained; further items
are counted as omitted. URL copies in output are capped at 2,048 characters,
with a SHA-256 identity instead. A frontier row whose stored URL exceeds 8 KiB is
omitted with a reason and no URL copy. Reasons and raw attribute values are also
bounded. These limits cover retained analysis structures, not total process
memory; SQLite, one row and serde buffers add overhead.

Legacy or partial rows are not promoted: an absent collector is `absent`, an
unavailable collector keeps its reason, an unsupported collector version or
invalid payload is `invalid`, and a run-budget marker is `unavailable` with
`run_byte_limit`. Duplicate collectors are rejected rather than selected. The
command does not rewrite old rows and does not require a collector added after
the crawl.

## Deployment order and feature gate

The command is available in the normal binary; it has no environment feature
gate and no deployment prerequisite because it does not add a wire or delivery
field. Existing local crawls already capture the required `resource` and
`resource_references` collectors for requested pages and resources, so this slice
adds no fetch or collection behavior. Deploying a newer binary does not rewrite
existing local databases. Old binaries can ignore the command; old data remains
readable with explicit unknown states. Do not claim S08, browser, field or
production readiness from these static local fixtures.