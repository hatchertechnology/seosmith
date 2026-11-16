# Wave 0 operations adapter decision

Implement `seosmith operations` inside the existing binary. Reuse CrawlRequest
validation, the SQLite store and public-address-only crawler. This adds no listener,
subprocess, shell, dependency or HTP requirement. Existing `crawl`, `serve` and
private `/v1` routes remain unchanged. There is no pre-existing CLI status/cancel
command to wrap. The adapter is new and must be capability-detected before use.

Each local audit gets an explicitly selected absolute database path, in a private
operator-owned directory, separate from service state. Start runs synchronously;
hosts can supervise it in their existing background execution facility. Concurrent
status/report/cancel do not acquire the worker lock or recover jobs. Start takes
the existing exclusive worker lock, validates explicit bounds before persistence,
and permits one immutable request per database. A retry only returns existing
state; it never resumes or creates another crawl. An interrupted worker leaves
unknown execution, not successful completion. A new intended audit uses a new
UUID and database. No automatic retry of uncertain mutations.

Reports are exported as original retained UTF-8 bytes, without projection through
an evolving Report type. Schema, versions, partial outcomes and coverage remain
producer-owned. No edits to model/audit contracts. Local delivery is disabled;
optional HTP orchestration remains separately authenticated and authorized.
