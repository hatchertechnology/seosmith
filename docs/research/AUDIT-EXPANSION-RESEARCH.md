# Additional audit coverage and comparison data

Research date: 2026-11-01. Planning document only; no audit behavior was changed.

## Basis and interpretation

Reviewed the published features of SEOnaut, Crawlie, SiteOne Crawler, LibreCrawl,
CrawlSEO, Unlighthouse, Screaming Frog, Sitebulb, DataForSEO and JetOctopus.
Compared them with the current working-tree rule catalog in `src/audit.rs`,
the report model in `src/model.rs`, and `COVERAGE.md` / `EXTRACTION.md`.
Existing uncommitted extraction improvements are included in this baseline.
Provider features are documentation claims, not results of hands-on tests.

The backlog below is our proposed design, informed by those products. It does
not imply every named product implements every proposed detail. Items marked
extension deepen existing observations; new items add a capability.

Priority: P1 = first implementation wave, P2 = next substantial coverage wave,
P3 = optional or specialized. Effort is relative engineering scope: S, M, L.
Priorities are recommendations, not commitments.

## What each project contributes

| Project / provider | Documented capability worth studying | Where it fits |
| --- | --- | --- |
| [SEOnaut](https://seonaut.org/features/) | Subdomain crawling, Basic Auth, configurable nofollow/robots behavior, WACZ archives | Explicit audit profiles and reproducible evidence; archive replay is optional |
| [Crawlie](https://github.com/spronta/crawlie) | Structured-data checks, accessibility, source/render observations, issue explanations, agent interfaces | Additional validators and remediation metadata |
| [SiteOne](https://github.com/janreges/siteone-crawler) | JSON reports, CI gates, browser diagnostics/screenshots, caching/TLS observations, passive technology detection | Browser evidence, regression gates, operational context |
| [LibreCrawl](https://github.com/PhialsBasement/LibreCrawl) | JavaScript rendering, PageSpeed integration, relationship maps and exports; [REST API](https://librecrawl.com/api/docs/) | Browser/performance adapters and portable comparison inputs |
| [CrawlSEO](https://github.com/crawlseo/crawlseo) | Search Console trends, opportunities, vitals, alerts and read-only MCP | HTP joins between audit findings and search outcomes |
| [Unlighthouse](https://unlighthouse.dev/) | Site-wide Lighthouse, mobile/desktop scans, history and CI budgets | Separate performance worker and reproducible performance comparisons |
| [Screaming Frog](https://www.screamingfrog.co.uk/seo-spider/) | Near duplicates, custom extraction, structured data, staging comparisons, mobile, spelling and authentication | Rule benchmarks and controlled before/after comparisons |
| [Sitebulb](https://sitebulb.com/hints/) | Detailed indexability, link, sitemap, rendering, accessibility and performance hints | Specific rule gaps and evidence-driven explanations |
| [DataForSEO](https://docs.dataforseo.com/v3/on_page-overview/) | Page/resource data, similar content, waterfall, content parsing and configurable thresholds | Optional external observations and independent comparison runs |
| [JetOctopus](https://jetoctopus.com/) | Crawl + logs + GSC/GA4, bot activity, segments and alerts | External evidence joins and business-impact prioritization |

## Existing coverage to extend, not duplicate

Seosmith already checks titles/descriptions, exact normalized-body duplicates,
heading order, image alt/declared size, indexability directives, redirect chains,
canonical/hreflang target states, sitemap noindex/blocked/noncanonical URLs,
candidate sitemap orphans, dependency errors, insecure resources and basic headers.
It already extracts JSON-LD/microdata and repeated Open Graph/Twitter values.
Those extractions are not schema or social-card validation. Its header timing is
not a browser performance measurement. Its current report already carries engine,
schema and ruleset versions, request settings and partial-coverage information.

## A. Coverage that can build on the HTTP crawler

| ID | Proposed coverage | Gap and evidence to retain | Priority / effort | Inspiration |
| --- | --- | --- | --- | --- |
| A01 | Structured-data validation | Extension: validate recognized types, property/value shapes, required vs recommended properties and unresolved local `@id` references. Retain item/property path, value, validator version and supporting documentation. | P1 / M | Crawlie; Screaming Frog |
| A02 | Social-card validation | Extension: detect conflicting singleton values, incomplete cards, unsupported card types, invalid URLs and observed broken preview images. Preserve type-specific requirements; repeated values can be valid. | P1 / S–M | Crawlie; Screaming Frog |
| A03 | Directive provenance and conflicts | Extension: retain HTML/header directive sets separately by applicable user agent; explain effective noindex/nofollow and contradictions. Interpret `googlebot` separately from generic directives. | P1 / M | Sitebulb indexability; CrawlSEO |
| A04 | Canonical syntax, location and cycles | Extension: empty/malformed canonical values, placement outside head, canonical-to-robots-blocked targets and explicit canonical cycles. Preserve source location and graph path. | P1 / M | Sitebulb indexability |
| A05 | Sitemap health and provenance | Extension: connect sitemap errors, redirects/timeouts and duplicate membership to their source sitemap. Preserve sitemap URL, entry, fetch outcome and discovery provenance. | P1 / M | Sitebulb sitemaps |
| A06 | Sitemap freshness observations | New: retain `lastmod`; compare invalid/future dates and historical unchanged-date/content-change patterns. Label mismatches as observations, not proof of dishonesty. | P2 / M | SiteOne; proposed extension |
| A07 | Near-duplicate main content | Extension: remove repeated template content, compare shingles/fingerprints and emit similarity clusters with matched excerpts. Keep exact duplicates separate; legitimate variants need context. | P1 / M–L | Screaming Frog; Sitebulb duplicates; DataForSEO |
| A08 | Duplicate H1/template patterns | Extension: group repeated H1s and title/H1 combinations by page type. Retain counts/examples and distinguish boilerplate from unique page content. | P2 / S–M | Sitebulb duplicates |
| A09 | Main-content measurements | Extension: retain main-text count/hash, boilerplate ratio, language-aware readability and heading/text structure. Present measurements before applying page-type thresholds. | P2 / M | DataForSEO content data |
| A10 | Soft-404 candidates | New: flag successful responses resembling known error templates, empty results or removed products. Retain status, matching signals and excerpts; require corroboration. | P2 / M | Proposed extension to content/response analysis |
| A11 | Anchor and fragment validation | Extension: empty/non-descriptive anchors, malformed hrefs and missing fragment targets. Preserve raw href, resolved URL, fragment, source element and accessible name. | P1 / M | Sitebulb links |
| A12 | Internal-link importance | Extension: unique followed inlinks, shortest navigational depth, link position, template vs contextual links and weakly connected clusters. Retain graph coverage and distinguish navigational reach from canonical discovery. | P2 / M | Sitebulb links; Screaming Frog architecture |
| A13 | URL-variant and crawl-trap candidates | New: cluster query/sort/filter/session and case/slash variants by observed similarity and canonical behavior. Record bounded samples; do not automatically treat all parameters as duplicates. | P2 / M–L | Sitebulb technical duplicates; proposed extension |
| A14 | Hreflang cluster consistency | Extension: compare complete observed alternate sets, duplicate language/region mappings and disconnected clusters; preserve per-target checked/unknown state. | P2 / M | Sitebulb international; Screaming Frog |
| A15 | Compression and cache policy | New: retain content encoding, encoded/decoded sizes, cache directives, validators and cache age; diagnose large compressible transfers and unsuitable static-asset policies. | P1 / M | DataForSEO pages; SiteOne; Crawlie |
| A16 | Asset impact and duplication | Extension: invert dependency graph to list affected pages per resource; detect identical assets at different URLs and aggregate resource bytes by type/origin. | P2 / M | DataForSEO resources; LibreCrawl |
| A17 | Image metadata and delivery | Extension: decode actual dimensions/format, compare declared dimensions, inspect responsive descriptors and retain lazy-load attributes. Oversizing relative to display requires browser evidence. | P2 / M | Screaming Frog; performance tools |
| A18 | Security/TLS detail | Extension: record certificate expiry/hostname observations, redirect downgrade and policy-header contents rather than presence alone. Keep security separate from SEO severity. | P2 / M | SiteOne; Sitebulb security |
| A19 | Static accessibility checks | New: missing labels, unnamed controls/links, iframe titles, invalid ARIA references, positive tabindex and zoom restrictions. Store element/path and criterion; automated checks do not establish WCAG conformance. | P2 / M | Crawlie; Sitebulb accessibility |
| A20 | Bounded custom extraction and assertions | New: configured selectors for SKU, price, author, timestamps, analytics identifiers or business-specific expected elements. Retain selector, values and policy version. Begin with declarative rules. | P2 / M | Screaming Frog custom extraction |
| A21 | Content diagnostics | New: optional language-aware spelling, readability and title/content similarity. Supply dictionaries/exclusions and supporting text; avoid default ranking-error labels. | P3 / M | Screaming Frog; DataForSEO |
| A22 | Authorship and machine-readable content | New: observe author/date/about/contact references, semantic sections and extractable answer blocks. Treat AI-search readiness as optional heuristics with evidence, never a guaranteed citation/ranking score. | P3 / M | Crawlie GEO |

Source detail for this table: [Sitebulb indexability](https://sitebulb.com/hints/indexability/),
[links](https://sitebulb.com/hints/links/), [sitemaps](https://sitebulb.com/hints/xml-sitemaps/),
[duplicates](https://sitebulb.com/hints/duplicate-content/),
[international](https://sitebulb.com/hints/international/),
[accessibility](https://sitebulb.com/hints/accessibility/),
[DataForSEO page fields](https://docs.dataforseo.com/v3/on_page/pages/).
Schema profiles must track current [Google requirements](https://developers.google.com/search/docs/appearance/structured-data/sd-policies);
valid markup does not guarantee a rich result, and eligibility includes conditions
a parser cannot establish.

## B. Coverage requiring browser execution or additional crawl modes

| ID | Proposed coverage | Evidence/data to add | Priority / effort | Inspiration |
| --- | --- | --- | --- | --- |
| B01 | Source-versus-rendered SEO comparison | Two separately identified observations of title, description, robots, canonical, headings, text, structured data and links; added/removed/changed values and render completion state. | P1 capability / L | Sitebulb rendered; Crawlie |
| B02 | Browser failure diagnostics | Console exceptions, failed requests, CSP/CORS failures and dependencies blocking meaningful content. Attribute failures to affected pages with bounded samples. | P2 / M after B01 | SiteOne browser mode |
| B03 | Mobile/desktop parity | Compare indexability, main content, canonical/hreflang, schema and navigational links by viewport/profile; include overflow and rendered image sizing. | P2 / M–L after B01 | Sitebulb mobile; Screaming Frog |
| B04 | Lighthouse lab diagnostics | LCP, CLS, FCP, TBT, score components, unused resources, render-blocking work and third-party cost. Store raw report and browser/device/throttling versions. | P2 / M–L | Unlighthouse; LibreCrawl |
| B05 | Rendered accessibility | Add contrast, focus/visibility and accessible-name diagnostics through a versioned browser audit engine. Preserve affected elements and incomplete checks. | P2 / M | Sitebulb; Lighthouse |
| B06 | Screenshot evidence | Store viewport/full-page images with URL, timestamp and render profile; optional image diffs need stable dimensions and masking for dynamic regions. | P3 / M | SiteOne; DataForSEO screenshots |
| B07 | Staging/authenticated audits | Explicit host allowlists, Basic Auth/session profiles, URL-list and sitemap-only modes; production-to-staging URL mapping and exact-list audit boundaries. | P2 / L | SEOnaut; Screaming Frog; SiteOne |
| B08 | Search-bot policy simulation | Evaluate robots/directives for named agents and compare relevant profiles. Preserve agent, policy snapshot and matched rule. A spoofed user-agent is not a faithful Google rendering environment. | P2 / M | JetOctopus robots tools; SEOnaut |

References: [Sitebulb source/render hints](https://sitebulb.com/hints/rendered/),
[mobile checks](https://sitebulb.com/hints/mobile-friendly/),
[Unlighthouse](https://unlighthouse.dev/),
[SiteOne browser documentation](https://github.com/janreges/siteone-crawler#-browser-rendering-optional),
[Screaming Frog comparisons](https://www.screamingfrog.co.uk/seo-spider/).

Browser work should be an isolated, opt-in worker with independent CPU/time/network
budgets. Preserve Seosmith's public-network protections for document navigation,
subresources, redirects and browser requests. Authentication must be scoped to
explicit hosts and kept out of reports. Supporting staging/private networks is a
separate trusted deployment policy, not removal of the public crawler's safeguards.

## C. External observations to join through HTP

| ID | Proposed coverage | Comparison and data needed | Priority / effort | Inspiration |
| --- | --- | --- | --- | --- |
| C01 | Real-user performance | URL/origin CrUX p75 LCP, CLS and INP, device and collection window; compare with lab runs without conflating them. Missing data stays unavailable. | P2 / M | CrawlSEO; LibreCrawl; PSI |
| C02 | Search Console URL inspection | Indexed status, Google-selected vs declared canonical, last crawl and reported coverage, with observation timestamp. This describes Google's stored version, not a live audit. | P2 / M | Search Console integrations |
| C03 | Search-performance prioritization | Join clicks, impressions, CTR and average position by URL/query/device/country/date; prioritize errors on valuable pages and identify high-impression low-CTR opportunities. | P2 / M | CrawlSEO; JetOctopus GSC |
| C04 | Search trend and overlap candidates | Compare equivalent periods for traffic decay and query/page overlap. Preserve query sets and filters; overlap alone does not prove harmful cannibalization or causation. | P2 / M | CrawlSEO; JetOctopus GSC |
| C05 | Analytics/business impact | Landing sessions, conversions and value by page, with currency, attribution/time window and consent/data-availability context. Keep observed impact separate from estimated opportunity. | P2 / M | Screaming Frog; JetOctopus |
| C06 | Verified bot-log analysis | Last visit, frequency, status mix, latency and visits to redirect/error/parameter URLs. Retain bot-verification method and distinguish claimed from verified identity. | P3 / L | JetOctopus log analyzer |
| C07 | Cross-source discovery/orphans | Union crawl, sitemap, GSC, analytics, CMS inventory and logs; explain where each URL was discovered and which sources cover the period. | P2 / M–L | JetOctopus; Sitebulb |
| C08 | Optional backlink/SERP data | Backlinks to broken or redirected URLs, competing landing pages and keyword opportunity snapshots; retain provider/date/location/device and usage cost. | P3 / M | CrawlSEO; DataForSEO |

References: [JetOctopus logs](https://jetoctopus.com/log-analyzer/),
[GSC reports](https://jetoctopus.com/gsc-integration/),
[CrawlSEO](https://github.com/crawlseo/crawlseo),
[PSI lab and field definitions](https://developers.google.com/speed/docs/insights/v5/about),
[Google URL Inspection API](https://developers.google.com/webmaster-tools/v1/urlInspection.index/inspect).
Bot requests do not prove indexing or AI citations. Zero impressions does not prove
non-indexing. Performance/traffic changes near a deployment do not establish causation.

## D. Comparison data and analysis to add

These are proposed Seosmith/HTP capabilities. They combine lessons from Screaming
Frog staging/crawl comparisons, Sitebulb audit comparison, SiteOne CI baselines,
Unlighthouse history/budgets and JetOctopus/CrawlSEO evidence joins.

| ID | Addition | Required behavior / stored data | Priority |
| --- | --- | --- | --- |
| D01 | Finding lifecycle | New, persisting, changed evidence/severity, resolved, reopened and not observed. Keep first/last observed, last checked and compatible baseline IDs. | P1 |
| D02 | Comparable-run signature | Extend existing versions/settings with normalized scope, discovery/follow policy, render/device/auth profile identifiers, validator versions, extraction limits and configuration digest. Never persist secrets. | P1 |
| D03 | Coverage-aware comparison | Per-URL and per-rule eligibility/attempt/result state: pass, fail, unknown, not applicable. Missing from a partial run must not become resolved. Show comparable and excluded denominators. | P1 |
| D04 | URL observation history | Preserve exact fetched URL plus a comparison key; track status, redirects, canonical, directives, headings, content hash, link sets and resource totals between compatible runs. Avoid destructive URL normalization. | P1 |
| D05 | Multiple finding identities | Preserve existing stable identity behavior while designing entity-level identity for schema items, multiple broken assets and multiple bad anchors on one page. Specify migration/versioning before replacing keys. | P1 |
| D06 | Content and graph changes | Main-content similarity delta, added/lost links, changed inlinks/depth, newly isolated pages and canonical/hreflang cluster changes. Retain matched evidence. | P2 |
| D07 | Staging/migration comparison | Explicit source/target host/path mapping; expected redirects and intended canonical/indexability changes; highlight missing content, schema and internal links. | P2 |
| D08 | Independent-tool comparison | Import authorized JSON/CSV exports from SEOnaut/Crawlie/SiteOne/LibreCrawl/Screaming Frog, or optional DataForSEO results. Normalize observations and map rules with confidence. Preserve originals and unsupported fields. | P1 |
| D09 | Fixture benchmark | Known expected outcomes for static, malformed HTML, redirects, bots, sitemaps, multilingual, schema and JavaScript cases. Measure precision/recall against labeled fixtures, not agreement with another tool. | P1 |
| D10 | Performance distribution | Repeated lab runs with sample count, median/spread and consistent environment; separately compare rolling field windows. Avoid flagging a single noisy timing difference as regression. | P2 |
| D11 | Segmented trends | URL pattern, page template, locale, device, content type, business importance and discovery source; show affected/eligible counts per segment. | P2 |
| D12 | Evidence and remediation metadata | Source locator, actual/expected value, related URLs, diagnostic explanation, confidence, fix guidance and reference URL; preserve immutable evidence artifact hashes. | P1 |
| D13 | Baseline-aware CI policy | Fail on new critical findings, lost indexable pages, broken migration mappings or defined budget regressions. Retain policy/version, thresholds and per-check results; support reviewed baseline exceptions. | P2 |
| D14 | Business-impact ranking | Show technical severity, confidence, affected reach, traffic/conversion exposure and estimated effort separately; allow an explainable prioritization formula. | P2 |

Cross-tool disagreement should be classified as scope, source/render mode, fetch
failure, parser/extraction difference, rule semantics, threshold or possible bug.
Do not compare proprietary aggregate health scores as if they use one definition.
Use the same fixture inputs, URL lists, crawl policies and observation window where
possible. Record crawl duration, fetched/discovered/failed counts, peak memory and
CPU for engineering comparisons, alongside detection accuracy.

## Suggested data contract

Evolve the current report additively where compatible; use a schema migration for
incompatible changes. The current rule catalog has severity and a short description;
extend it with applicability, evidence schema, explanation and validator provenance.

- Observation: URL, observation time, source/provider, fetch/render profile, relevant
  response headers, extracted values, attempt/outcome, limit flags and artifact hashes.
- Evaluation: rule/version, entity/locator, eligibility, pass/fail/unknown/not-applicable,
  actual/expected, related observations, confidence and evidence.
- Comparison: two immutable report IDs, comparison-policy version, scope compatibility,
  mapped URL/entity identity, field/finding deltas and excluded/incomparable records.
- External data: source account/property identifier, URL/query, device/country filters,
  collection interval, fetched-at time, freshness, URL-vs-origin granularity and missing
  data reason. Keep original provider metrics alongside normalized values.

Keep raw report persistence/delivery immutable. Derive historical comparisons in HTP
or a dedicated analysis layer; do not rewrite a report when new external data arrives.
HTP should own analytics credentials, cross-run history, business joins, notifications
and user-facing presentation. Seosmith should own crawl evidence and deterministic
evaluations. Browser/performance workers can produce separately versioned evidence.

## Recommended implementation order

1. Comparison foundation: D01–D05, D08–D09 and D12. Add observation-level coverage
   states, a fixture corpus and import mappings before using other tools as benchmarks.
2. HTTP coverage: A01–A05, A07, A11 and A15. These deepen existing data without
   making every audit depend on a browser or paid service.
3. Browser worker: B01 first, then B02–B05 with bounded representative URL selection,
   independently reported rendering coverage and versioned diagnostics.
4. Historical/impact analysis: D06, D10–D14 and C01–C05/C07 in HTP; reuse existing
   integrations after checking their actual fields, permissions and freshness.
5. Specialized options: authenticated/staging modes, bot logs, backlinks, editorial
   diagnostics and optional AI-search heuristics according to demonstrated demand.

Avoid making WACZ replay, site cloning, load testing, keyword databases or one
combined SEO/security/accessibility/GEO score prerequisites for the audit engine.
Archives, MCP access and exports may help workflows but do not by themselves add
audit coverage. Keep intentional noindex/canonical/robots choices configurable as
expected policies; do not import a competitor's severity labels uncritically.
