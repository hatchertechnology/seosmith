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

The report includes source and label SHA-256, provenance, individual cases and raw unmapped findings. Keep the original export and label files: hashes identify them but do not replace source evidence. Inputs are limited to 16 MiB each. Missing pages, failed/non-HTML/truncated pages, incomplete/failed reports, absent CSV rows and explicit unknowns are unknown, never negatives. Unmapped rules are retained separately. Known truth and observation pairs alone enter TP/FP/TN/FN; unknown truth and observation are counted separately. Precision is TP/(TP+FP), recall TP/(TP+FN); zero denominators produce null. These are conditional on evaluated cases, so review counts and coverage before interpreting scores. A comparison describes differences against labels, never another crawler's score as truth.

Before mapping another crawler, record its exact version, export, configuration, scope, render mode, parser, thresholds, fetch outcomes and rule definitions. Document each rule's semantic equivalence or partial/unmapped status; do not manufacture negatives from omitted rows. The current CSV has no implicit mapping and cannot establish those facts for you.

## Measurements and limits

`measure.py` runs only the offline comparison in a child with a 1–60 second wall/CPU limit and 64–4096 MiB address-space limit (defaults 10 seconds and 1024 MiB). It records elapsed monotonic time, Linux `wait4` child peak RSS, exit status and bounds. This includes Python startup and file I/O, not crawl time. Re-run on the same machine and inputs for comparison; scheduling and caches vary. Fetch and discovery counts are explicitly null: this harness does not instrument a crawler. It cannot measure crawler runtime, fetch/discovery or crawler memory until a controlled instrumented execution exists; no production crawl or credentials are needed. A limit failure has nonzero exit and is not a performance result.

## Contract and rollout

The independent S01 contract checker is separate, currently under review; run it once merged and do not treat this comparison as schema validation. Unknown versions are rejected. This harness does not write H11 comparison storage, change producer/receiver contracts, enable a feature flag, or deploy anything. After compatible S01 receiver/read surfaces ship, preserve raw exports and provenance before enabling any downstream import; rollback is to stop running/importing the harness. S15 remains limited until S01 merges, controlled crawler exports and crawl instrumentation are available, and H11 handles storage.
