# S06 Wave 1: local fixture and test design

Scope is the live [S06 issue](https://github.com/hatchertechnology/seosmith/issues/6), read 2026-11-21, while the S14 delivery contract remains pending. This corpus adds no scanner, wire, rule registry, capability, feature gate, receiver or production changes. No AGENTS.md exists in this checkout or its ancestors; the live repository root contents request also returned 404 for AGENTS.md. Coordination instructions apply to the external handoff.

## Local conventions and labels

Like S15, `fixtures/s06/labels.json` retains synthetic `fixture.invalid` URLs, relative HTML files, a local version/semantics identifier, case families and nullable expected labels. S06 is separate from S15: its boolean means **candidate for human review**, not a missing-metadata finding, penalty, spam determination or production rule result. `false` means no candidate for this hand-authored example; it does not prove overall page quality. `null` means unknown, never a pass. `review_state` makes this distinction explicit. These fields are corpus annotations, not proposed wire fields or a contract freeze.

Each case includes a rationale, modeled response status/content type/availability, a primary source SHA-256 and related retained HTML with SHA-256 hashes where applicable. These are modeled responses, not real HTTP captures. The committed bytes are authoritative; hashes identify them without replacing evidence. No JavaScript runs, no script asset is fetched and no rendering outcome is invented. Pair URLs are not canonicalized or conflated. `unavailable_related` explicitly records an uncollected comparison peer.

| Cases | Review label and reason |
| --- | --- |
| product-variants | No candidate: legitimate color and SKU choices despite similar text |
| useful-hours / useful-contact | No candidate: short pages answer a specific intent |
| error-with-success | Manual review: missing-item title/H1/body served as modeled 200 |
| real-not-found | No soft-404 candidate: identical error body with modeled 404 |
| error-phrase-guide | No candidate: useful guide mentions error wording |
| empty-shell / loading-shell | Unknown: static shell cannot establish missing resource or rendered content |
| location-variants | Manual review: repeated service text with only location substitution |
| shared-template | No candidate: common navigation/footer with distinct main content |
| exact-duplicate | Manual review: byte-identical pages, purpose unresolved |
| fetch-failed / robots-blocked / omitted-page / missing-status | Unknown: required source unavailable |
| truncated-page / server-error / non-html | Unknown: incomplete or ineligible evidence |
| unavailable-pair | Unknown: missing peer cannot establish uniqueness |

Labels are author judgments over these controlled examples, not independently adjudicated accuracy ground truth. H04/H05 must supply purpose, intent and outcomes before business interpretation. Even a legitimate duplicate may remain a factual duplicate observation without becoming an advisory or violation. App-shell availability must remain distinct from suspected soft-404 evidence; optional rendering is a later separately scoped capability.

## Executable checks now

```sh
python3 -m unittest discover -s tests/s06 -v
python3 -m unittest discover -s tests/s15 -v
```

The S06 checks pin the full case and peer inventory, provenance, primary and related retained bytes, duplicate/status and shared-template controls, source-less cases and unavailable peers, and review/unknown boundaries. The existing Rust backend CI workflow runs this Python corpus suite on pushes and pull requests. They do not run a classifier, extraction algorithm, wire validator or crawler, and cannot establish precision, recall or production readiness. S15's existing harness maps only metadata rules; do not feed S06 labels into it or count unmapped S06 observations as negatives. No new dependencies are required beyond Python's standard library.

## Deferred implementation test design

After S14 semantics are agreed, build tests using the existing parser/audit fixture pattern (`tests/s15_fixtures.rs`) and controlled in-memory Transport (`examples/s15_controlled.rs`), without relaxing the production fetcher. Keep the present fixtures as retained source evidence and add independently reviewed cases before choosing thresholds.

1. Extraction: assert exact retained source hash separately from normalized main-text fingerprint; retain extraction method/version, template proportion, title/H1 patterns, error signals and bounded excerpts with locators. Compare shared-template pages to ensure chrome does not dominate similarity. Add Unicode, whitespace, malformed HTML, missing main landmarks and excerpt-boundary cases once extraction semantics and bounds are defined. Never assume `<main>` covers all sites.
2. Similarity: exact-copy control should yield factual equality; location variants should yield a review candidate, product variants should preserve legitimate intent, and unrelated main text should remain distinct. Test pair ordering, deterministic cluster identity/membership, singleton and missing-peer handling, threshold equality and corpus-size limits only after versioned algorithm/threshold decisions. Avoid snapshots of arbitrary floating-point scores now.
3. Soft 404: use identical error bytes at 200 versus 404 to isolate status; combine title/H1/body evidence rather than match a single error phrase. Useful error-help and short pages are negative controls. Shells stay unknown without rendered evidence; do not turn missing main text, script presence or low word count into an error verdict.
4. Coverage: exercise missing status, non-success, non-HTML, robots denial, fetch failure, truncation and omitted peers in the controlled transport. Assert unknown with explicit reason and retained evidence; an omitted record or failed scan must never imply a completed negative. Add partial-run and mixed-pair cases so coverage cannot silently inflate.
5. Contracts/readers: only after agreement, add paired producer/receiver/read fixtures with source pointers, versioned semantics, provenance, advisory/manual-review wording and explicit unknowns. Test legacy exact-hash findings independently so new main-text semantics do not silently reinterpret historical reports. Unknown algorithm versions must not compare as equivalent.

## Dependencies, rollout and rollback

S06 is blocked by [HTP6](https://github.com/hatchertechnology/htp/issues/6) and [S14](https://github.com/hatchertechnology/seosmith/issues/14); it blocks S07 and belongs to HTP22. H04/H05 own contextual interpretation. Existing `s14-local-1` is locally frozen only; the wire prototype/candidate is not approval to deliver richer S06 evidence. Affected future surfaces include producer page/evidence models, advisory registry and policy, receiver validation, persisted history, H04 display and downstream S07 clustering inputs. No schema, field mapping, version number or gate activation is chosen here.

Future order: agree versioned semantics and paired vectors; implement compatible receiver/storage/read surfaces; verify H04 human-review/unknown presentation; implement a separately gated producer; verify end-to-end evidence and rollback before activation. Retain legacy interpretation. Rollback must stop new producer data while readers keep supporting already retained reports; fixture rollback is simply to stop using/remove this local corpus. Production thresholds, wire delivery, capability advertisement and issue closure remain deferred. This local slice needs no deployment.
