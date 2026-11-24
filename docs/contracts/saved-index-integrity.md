# Saved effective indexing and integrity readout

`seosmith --database PATH saved-index-integrity REQUEST_ID` is the ordinary
read-only CLI for a terminal local-only checkpoint. It reuses the merged
`robots_evidence`, `index_evidence`, `integrity_evidence`,
`sitemap_evidence` and `graph_evidence` analyzers. It does not fetch, crawl,
recover, migrate, write a report, queue delivery or modify an existing report.

Use it only on a quiesced offline snapshot. The command refuses a WAL or
rollback journal and opens SQLite with immutable read-only mode. A concurrent
writer after the sidecar checks cannot be detected. Missing databases are
rejected without creation.

## Output

The JSON version 1 document reports:

- terminal job state, finalization origin and `discovery_coverage: unknown`;
- explicit freshness metadata: the retained run start/created time, no
  per-page observation time, and the rule that sitemap `lastmod` is syntax,
  not a measured freshness claim;
- a coverage record with reasons and the applied bounds;
- each retained page, including blocked and failed pages, its HTTP facts,
  collector states, explicit evidence reasons and bounded omissions;
- effective crawl permission, index/follow decisions, conflicts, restrictions,
  desired-policy unknown and the contributing occurrence locators for
  Seosmith, Googlebot, Bingbot, GPTBot and OAI-SearchBot;
- canonical declarations and agreement states, retained canonical paths with
  terminal, cycle, blocked, redirect, error, noindex, unavailable,
  out-of-scope, invalid and limit states;
- alternate/hreflang declarations, retained-cluster issues and reciprocity;
- retained sitemap roots, nested document paths, every index-hop ordinal, leaf
  entry ordinal, parse state and raw `lastmod` state;
- navigation reachability from the saved link graph;
- findings with an explicit `path` and `source`, plus positive, negative and
  uncertain kinds. A finding is omitted only when the finding budget is
  exhausted, and the omission count is reported.

Canonical graph target state uses the Seosmith index observation. Per-bot
effective indexing remains independent in each page record.

## Trust and limits

Terminal `local_only` does not prove that discovery completed. `completion`
reports the stored finalization origin (`completed`, `interrupted` or
`unknown`), while discovery remains unknown even for a completed job. Missing
or legacy evidence is unavailable, never treated as an empty observation.
Blocked content does not imply a noindex directive. Sitemap entry paths are
conditional on retained documents and explicit errors and omissions.

The reader accepts at most 1,000 frontier rows, 64 KiB of evidence per row,
512 KiB of saved page JSON per row, 2 MiB of aggregate parsed evidence, 1,000
omission records, 2,000 findings, 2,000 sitemap origin lookups and a 4 MiB JSON
output. A row limit is reported. Excessive final output fails without partial
JSON. Memory use includes SQLite, one bounded row, parsed page/graph structures
and bounded output construction; these limits are not a total RSS bound.

Existing report bytes, report hashes, delivery state and legacy schemas are not
rewritten. Older terminal snapshots without `local_finalization` are read with
completion `unknown`. Unsupported pre-mode job schemas are rejected without
migration. Rollback is to stop invoking the command; older binaries and saved
reports are unaffected.

This is a local readout for SEOsmith #2 and #3 evidence. It is not an S02/S03
completion claim, provider indexing observation, live-site freshness proof or
wire/receiver contract.