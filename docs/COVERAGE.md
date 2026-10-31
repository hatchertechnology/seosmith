# Audit scope and SEOnaut comparison

Seosmith implements the backend crawl/report role; HTP already supplies websites,
accounts and history, so SEOnaut's signup, account management, templates, live UI,
archive replay and MySQL schema are intentionally not reproduced.

## Available rule families

- Missing/short/long/multiple/duplicate titles and descriptions.
- HTTP errors, redirects, observed redirect chains/cycles and failed fetches.
- Noindex, nosnippet, noimageindex and robots exclusions.
- Missing H1, skipped heading levels, missing lang, content size, duplicate
  normalized body text, DOM size, duplicate IDs, viewport and meta in body.
- Internal link errors/nofollow, follow/nofollow incoming conflicts, observed
  indexable nofollow targets, link counts, dead ends and localhost/HTTP links.
- Optional observed external redirects/errors; external pages are not expanded.
- Image alt/size/picture structure; same-host image byte size observations.
- Canonical multiplicity/relativity/header disagreement, errors, redirects,
  noindex targets and canonical chains.
- Hreflang self-reference, language mismatch, x-default, relative URLs, observed
  return links, conflicting language labels and target state/canonical issues.
- Sitemap noindex/blocked/noncanonical URLs and candidate orphan pages.
- URL underscores/spaces/slashes/depth; forms over HTTP/insecure actions;
  security header presence; common extension/media-type mismatches; slow headers.
- Pagination head links without corresponding body navigation.

The CLI/API rule catalog is authoritative. Severities and heuristic thresholds
are Seosmith policies, not Google ranking factors or guarantees. TTFB includes
connection/TLS time until headers; it is not a browser Core Web Vitals measurement.
An external link without nofollow is not intrinsically an SEO error and is not
flagged merely because it is followed. Language validity is not yet fully checked
against the BCP47 registry. Large-image observations cover fetched same-host src
resources, not every srcset candidate or externally hosted image.

## Coverage semantics

`completed` means the bounded discovered frontier was exhausted without observed
fetch/sitemap failures or explicit resource limits. It does not prove that every
page on the website is discovered, every real visitor can access it, or Google
has indexed it. Robots exclusions remain reported as exclusions.

`partial` identifies page/time/discovery/extraction limits or fetch/sitemap gaps.
Orphans are candidates within the observed graph. Unfetched canonical/hreflang
and external targets remain unknown rather than passing or failing. HTP must not
resolve an old finding just because its page disappeared from a partial run.
Compare compatible URL scopes, rulesets and configurations.

HTML is parsed without JavaScript. A client-rendered website can produce sparse
or misleading source-HTML audits. Browser-rendered crawling and authenticated
site crawling are not implemented. Exact-host scope does not automatically equate
`www` and bare domains; register the desired hostname explicitly.

JSON/CSV/sitemap generation can be provided in HTP from normalized reports; only
JSON machine reports ship here. WARC/WACZ archives and visual replay are outside
this metadata-first backend. No broad keyword or backlink index is included.
