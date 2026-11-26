# s14-schema4-v1

Paired schema-4 fixtures: the exact bytes the producer stores and delivers
(`finding_coverage_prototype::generate_wire_v1`: raw `audit::analyze`, policy,
evidence over the final findings, then `finding_coverage`). Nothing is
hand-edited. Not frozen.

Regenerate (the crawl case runs the real `seosmith crawl --policy-wire-v1`):

    cargo +1.98.1 build --locked --features local-cli-fixtures --bin seosmith
    cargo +1.98.1 run --locked --features local-cli-fixtures --example make_s14_schema4_fixtures
    python3 scripts/verify-s14-schema4.py --admit <htp>/target/debug/examples/admit --write-admit

`crawl.json` carries wall-clock timestamps, so regenerating changes its bytes
and digest; every other file is deterministic. `manifest.json` records bytes,
sha256, finding and evaluation counts, `evidence_contract.truncated`, and (after
`--write-admit`) the receiver's `validator_revision` and summary digest.

- `exact-cap.json` is exactly 4,952,756 bytes (`HTP_MAX_REPORT_BYTES`).
- `crawl.json` records the N-M4 numbers: evaluations <= 460 and not truncated.
- `mixed-caps-exceeded.json` exceeds MAX_OBSERVATIONS, so `truncated` is true.

Checks: `python3 scripts/verify-s14-schema4.py` and `cargo test --test s14_schema4_fixtures`
(manifest digests, cap, and the E-p ID-equality over the serialised bytes).
