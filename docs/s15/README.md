# S15: offline labeled comparison (Wave 0)

This is a deliberately small fixture benchmark: two metadata rules, each with a positive and a negative HTML page, plus one unavailable case. Labels express whether the issue exists in these specific static 200 HTML responses. They are hand-authored from the source HTML, not inferred from crawler consensus. The Rust fixture test runs the existing parser and audit on these bytes without a network. The synthetic CSV is an example of the interchange format, not the result of a crawler and not evidence of product accuracy. No production quality claim follows from these cases.

Run from the repository root (Python 3 and Cargo):

```sh
cargo test --test s15_fixtures
python3 -m unittest discover -s tests/s15 -v
python3 scripts/s15/compare.py --labels fixtures/s15/labels.json --export fixtures/s15/controlled.csv --adapter controlled-csv-v1
python3 scripts/s15/measure.py --labels fixtures/s15/labels.json --export fixtures/s15/controlled.csv --adapter controlled-csv-v1
```

## Import and interpretation

`seosmith-v1` reads the actual schema 1 retained report shape from `src/model.rs`: `pages` and `findings`, with engine/ruleset versions, outcome, stop reason and coverage. The operations adapter in PR #19 exposes those exact retained bytes; it is not required to run this harness. Supply an independently captured, controlled fixture export with `--export`. No actual vendor export or third-party mapping was available in this repository, so no external crawler adapter or interoperability is claimed. `controlled-csv-v1` is a manual interchange, requiring explicit `url,rule,state` rows; `state` is `present`, `absent`, or `unknown`. Only assert absence when the source actually evaluated that exact rule and URL. Match URLs exactly, without invented canonicalization.

The report includes source and label SHA-256, provenance, individual cases and raw mapped and unmapped evidence. Keep the original export and label files: hashes identify them but do not replace source evidence. Inputs are limited to 16 MiB each. Missing pages, failed/non-HTML/truncated pages, incomplete/failed reports, absent CSV rows and explicit unknowns are unknown, never negatives. Unmapped rules are retained separately. Known truth and observation pairs alone enter TP/FP/TN/FN; unknown truth and observation are counted separately. Precision is TP/(TP+FP), recall TP/(TP+FN); zero denominators produce null. These are conditional on evaluated cases, so review counts and coverage before interpreting scores. A comparison describes differences against labels, never another crawler's score as truth.

Before mapping another crawler, record its exact version, export, configuration, scope, render mode, parser, thresholds, fetch outcomes and rule definitions. Document each rule's semantic equivalence or partial/unmapped status; do not manufacture negatives from omitted rows. The current CSV has no implicit mapping and cannot establish those facts for you.

## Measurements and limits

Build and run the controlled crawler executable on Linux:

```sh
cargo build --example s15_controlled
python3 scripts/s15/benchmark.py --binary "$PWD/target/debug/examples/s15_controlled" --report /tmp/s15-report.json
S15_BENCH_BINARY="$PWD/target/debug/examples/s15_controlled" python3 -m unittest discover -s tests/s15 -v
```

The example links the actual SEOsmith crawler, parser, auditor and SQLite store, using a deterministic in-memory `Transport` with robots, a sitemap, duplicate links, a robots-blocked URL and a three-page budget. It never calls the network or weakens the production fetcher, which rejects private and loopback destinations. This measures the engine in a dedicated executable, **not** the production CLI, HTTP stack, DNS, or real network performance. The request uses 1 second delay and a 20 second crawl limit. Run from a freshly built binary and a fresh temporary database; record binary hash, host, build profile and toolchain for comparisons. Time varies with scheduling and caches. The JSON output is not the stored report's original byte representation; `--report` creates a new serialized report for inspection. Its timestamps and hash vary by run.

`fetch_attempt_count` counts every instrumented transport call, including robots and sitemap, with URL sequence preserved; duplicates are attempts, not unique URLs. `frontier_unique_including_seed` is the count of distinct SQLite frontier rows, including the seed and unobserved pending URLs; it is not a count of discovery calls. `completed_observations` counts report page records, including robots-blocked records that were never fetched. Frontier rows show sitemap membership and whether a page record exists. Partial outcome and page-limit stop are expected and must not be interpreted as complete coverage. The process records elapsed monotonic wall time and Linux `wait4` peak RSS, bounded by 5–60 seconds, a 128–4096 MiB address-space limit, a 16 MiB per-file limit and the fixed crawl request. Nonzero exit, timeout or limit failure is not a valid measurement. This runner requires Linux/POSIX resource and wait4 support; peak RSS units and process limits are platform-specific and unsupported platforms must report them unavailable rather than extrapolate.

`measure.py` separately measures only the offline comparison process, including Python startup and file I/O; its null fetch/discovery values refer to that process, not the crawler benchmark. No vendor crawler or production crawl is involved.

## Contract and rollout

The independent S01 contract checker is separate, currently under review; once merged run `node scripts/check-contract-vectors.mjs` from the repository root against its bundled contract vectors. It is not a validator for arbitrary comparison exports; do not treat this comparison as schema validation. Unknown versions are rejected. This harness does not write H11 comparison storage, change producer/receiver contracts, enable a feature flag, or deploy anything. After compatible S01 receiver/read surfaces ship, preserve raw exports and provenance before enabling any downstream import; rollback is to stop running/importing the harness. S15 remains limited until S01 merges, controlled crawler exports and crawl instrumentation are available, and H11 handles storage.
