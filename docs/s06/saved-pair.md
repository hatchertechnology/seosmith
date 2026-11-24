# S06 saved pair comparison

`seosmith saved-pair` is an ordinary local CLI entry point for bounded cross-page
comparison. It never crawls, fetches, renders, mutates the database, writes a
report or queues delivery. It exists because local checkpoints retain per-page
soft-404 assessments but not cross-page candidates.

## Modes

### Saved checkpoints

```sh
seosmith --database saved.sqlite saved-pair \
  --request-id 00000000-0000-0000-0000-000000000000 \
  --source https://fixture.invalid/east \
  --peer https://fixture.invalid/west
```

The database must be a quiesced, immutable snapshot of a terminal local-only
job (`local_only`, `local`, no WAL or rollback journal). Both URLs must be
distinct absolute URLs of at most 8 KiB. The command reads the fixed
`soft_404` collector from `local_page_evidence` and emits:

- explicit source and peer URLs plus URL SHA-256 identities;
- each side's crawled HTTP status, checkpoint state (`observed`, `partial` or
  `unavailable`) and reason;
- the retained `s06-local-detector-2` per-page assessment: decision, reason,
  semantics, source/main SHA-256, title, H1, bounded excerpt and template
  fraction;
- job terminal state, execution mode and completion provenance
  (`completed`, `interrupted` or `unknown`);
- an explicit comparison decision and the omissions that limit it.

Saved evidence does not include main text, so a pairwise similarity score cannot
be recomputed without inventing evidence. Consequently:

- identical source bytes across distinct URLs are a `candidate` only when both
  sides were complete HTTP 200 responses with available main content, matching
  the detector's readiness gate; a saved side whose frontier status is not 200
  (for example an identical 404/410 error page) is `unknown` with reason
  `saved_side_not_http_200`, never a byte-equality candidate;
- equal main-text hashes are reported as an observation, not a candidate,
  because the saved excerpt cannot prove the detector's token-support and SKU
  gates;
- all other pairs are `unknown`, never `no_candidate`;
- an unavailable, partial, legacy, incompatible-semantics, integrity-mismatched
  or budget-omitted side is `unknown` with that side and reason named;
- a per-page soft-404 `candidate` is provenance only. It does not become a pair
  candidate and a pair result does not reinterpret it.

The command rejects active or standard jobs, missing URLs, same-URL comparison
and oversized URLs. Legacy, unbound, tampered or budget-omitted checkpoints are
reported as unavailable sides with explicit reasons rather than compared.

### Retained HTML

```sh
seosmith saved-pair \
  --source-html fixtures/s06/service-east.html \
  --peer-html fixtures/s06/service-west.html \
  --source-status 200 --peer-status 200
```

This mode calls the existing `s06_detector::compare` on caller-supplied files.
It performs the full pairwise extraction and similarity path, including the
experimental 0.8 Jaccard threshold, SKU suppression, shared-chrome exclusion,
short-page token-support guard and loading/missing-main gate. The caller must
supply both statuses because the CLI cannot infer them; content type defaults to
`text/html`. Each file is read to at most 256 KiB + 1 byte so the detector can
report its own source limit, and both file identities and hashes are emitted.
Legitimate product variants and short useful pages therefore stay
`no_candidate`, repeated location/service text becomes a review `candidate`,
byte-identical pages report `identical source bytes`, and shells/missing
statuses remain `unknown`.

## Bounds, provenance and unknowns

- 8 KiB URL copy limit; distinct source and peer.
- 64 KiB saved evidence envelope limit per side.
- 256 KiB retained HTML input bound; 256 KiB total CLI output bound.
- 32 KiB retained extracted text and 240-character excerpts, as enforced by the
  detector.
- Read-only immutable database access; no sidecar creation, report or outbox
  writes.
- No raw HTML is persisted; retained HTML exists only in caller-provided files
  for the duration of the process.
- No network access in either mode.
- `unknown` is distinct from `no_candidate`, and every unavailable side names a
  reason or omission.

## Semantics, rollout and limits

The emitted `semantics` is `s06-local-detector-2`. Mismatched stored detector
semantics make the saved side unavailable instead of being compared. This local
CLI does not change report schemas, the S14 wire contract, delivery, scanner
registration or any feature gate. No receiver, H04/H05 presentation or
production activation is included.

Deployment order for any future producer remains: agree versioned S14 semantics
and paired vectors, deploy compatible receiver/storage/read surfaces, verify
H04 human-review and unknown presentation, then gate a producer. Rollback of
this slice is removal of the CLI/read path; stored checkpoints and readers stay
intact. Issue #6 remains open: this is a local, provisional signal, not
production readiness, accuracy evidence, a penalty determination or a final
pair-clustering contract.

Checks: `cargo test --locked --test s06_saved_pair`,
`cargo test --locked --bin seosmith saved_pair`, and the existing S06 detector
and Python corpus checks. The retained-HTML test exercises the product binary
with the committed synthetic fixtures. No third-party site is crawled.