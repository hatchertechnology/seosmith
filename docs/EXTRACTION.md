# Source extraction and HTP contract (engine 0.2.0)

HTP integration: [issue #3](https://github.com/hatchertechnology/htp/issues/3), implemented by
[merged PR #4](https://github.com/hatchertechnology/htp/pull/4). Receiver support has
been deployed; the collector release is separate.

The additions use schema_version 1 and ruleset_version `2026-11-01.1`.
Existing fields, delivery envelopes, exact-byte SHA-256 receipts, website scope,
and immutable report semantics are preserved. HTP must tolerate additive JSON
fields and historical records without them. No user interface is added.

## Crawl and dependency observations

`request.max_pages` limits page/navigation observations; `request.max_resources`
(default 500, range 0..20,000) independently limits assets and optional external
link observations. A zero resource budget records dependencies without fetching
them and makes the run partial if resources remain queued. `resource_limit` and
`page_limit` indicate which frontier remains. Both categories are retained in the
existing `pages` array; `Page.is_resource` identifies resource observations.

Canonical (including HTTP Link), hreflang and meta-refresh targets are queued
within the configured exact-host scope. Public CDN resources can be fetched
without `check_external_links`; that option governs navigational external links.
Resources include img src, img/picture srcset candidates, scripts, stylesheets,
iframes, audio/video sources, video posters, preload/modulepreload and CSS url()
and @import dependencies. CSS dependencies resolve against the stylesheet URL.
Resources do not recursively expand HTML navigation. HTTP redirects of resources
are queued with their resource classification. All requests use the existing DNS
pinning, public-address checks, robots policies, deadlines and response limits.

Each page has `relationships` entries:

```json
{
  "kind": "image_candidate",
  "url": "https://cdn.example/image.webp",
  "descriptor": "1200w",
  "resource": true,
  "state": "checked"
}
```

The enclosing page supplies the source URL. Kinds include navigation, canonical,
canonical_header, hreflang, hreflang_header, meta_refresh, image, image_candidate,
script, stylesheet, preload, modulepreload, iframe, audio, video, source,
video_poster and css_url. Descriptor carries candidate width/density, hreflang
language, or navigation rel values as appropriate. States are `checked`, `blocked`,
`fetch_failed`, `outside_scope`, or `not_fetched`. Checked means an HTTP response
was observed; look up the target's status for success/error. Unfetched targets do
not pass or fail target-state rules. Dependencies belonging to stylesheets appear
on the stylesheet observation, allowing HTP to traverse back to affected pages.

Links retain separate `nofollow`, `sponsored`, `ugc` booleans and tokenized `rel`.
All three discourage automatic navigation discovery, but only an explicit
nofollow sets that boolean. Historical checkpoints cannot recover rel distinctions
that were discarded by an earlier engine.

## Structured and social data

`structured_data` is an ordered array of `{format, data, error}` observations.

- `json_ld`: data preserves JSON objects, arrays, @context and @graph. Invalid JSON
  records a sanitized line/column error; scalar top-level values record
  `expected_object_or_array`. No remote contexts are fetched, scripts executed,
  JSON-LD expanded, or schema.org/rich-result compliance claimed.
- `microdata`: each top-level itemscope produces an object containing `types`
  (itemtype tokens), `id` (resolved itemid or null) and `properties`. Property
  values are arrays, including repeated/multiple itemprop names. Nested items are
  objects with the same shape. itemref targets are supported with cycle guards;
  URL-valued attributes resolve against the document base URL. This is source
  extraction, not browser-rendered or RDFa extraction.

Example:

```json
{"format":"microdata","data":{"types":["https://schema.org/LocalBusiness"],"id":null,"properties":{"name":["Client"],"address":[{"types":["https://schema.org/PostalAddress"],"id":null,"properties":{"streetAddress":["123 Main"]}}]}},"error":null}
```

`open_graph` and `twitter` are maps from lowercased complete tag names to arrays
of decoded content strings. Duplicate tags preserve document order (e.g. multiple
og:image entries and related og:image:width entries). Values remain source values;
HTP can resolve URL-valued social properties using the page URL. No missing-social
metadata or missing-structured-data errors are invented.

## Parsing, findings and limits

Character decoding honors BOM, Content-Type charset, then early HTML charset or
http-equiv declarations; UTF-8 is the fallback. `encoding` records the decoder;
decoding replacements produce `encoding_errors`. HTML extraction supports
successful 2xx text/html, application/xhtml+xml and application/vnd.wap.xhtml+xml.
Language falls back to Content-Language; `language_source` records html or
http_header. HTTP Link alternate/hreflang and canonical values are retained;
HTML canonical takes precedence while disagreement is still audited.
`refresh_delay` and `refresh_target` preserve HTML refresh separately from the HTTP
redirect field. Malformed refresh directives are not followed.

Findings keep stable per-rule/per-page keys and the original `evidence` string.
`evidence_items` retains distinct observed instances, such as every broken target
(up to 2,000 per rule/page). `details` carries observed status, bytes, ttfb_ms,
words, dom_nodes and observation_count. New rules: invalid_language,
invalid_hreflang, invalid_json_ld, meta_refresh, broken_resource,
insecure_resource, encoding_errors. Resource HTML is excluded from ordinary
page-metadata and duplicate-content rules.

Extraction bounds: 2,000 relationships and collection elements; 64 structured
blocks per page; 64 KiB per JSON-LD block; 256 KiB combined retained structured and
social metadata; 128 values per social map, 4,096 characters per value; microdata
2,000 visited nodes and nesting 16; CSS 20,000 tokens and nesting 16.
`extraction_truncated` and extraction_limit flags make bounded extraction partial.
Legacy HTML checkpoints without encoding/new extraction evidence make resumed runs
partial (`legacy_checkpoint_metadata`); start a fresh request for full new-engine
coverage. SQLite adds the frontier resource column automatically.

The existing 16 MiB whole-report limit is retained. Oversized reports are not
silently truncated or acknowledged; use smaller crawl scopes until a coordinated
chunk/object-storage protocol is implemented. The expanded HTP receiver accepts 20,000 page observations plus 20,000 resource
observations (40,000 combined), and 400,000 findings, within the same 16 MiB byte
limit. HTP separates page/resource summaries, recognizes XHTML/2xx, and uses
summary version 3 to distinguish intentional navigation exclusions from incomplete
evidence. Robots-blocked observations conservatively prevent HTP comparisons even
when the collector exhausted its configured frontier; the original report stays
unchanged. Target-dependent and HTML-specific findings require eligible current
evidence before HTP counts them resolved.

Run the real producer/receiver compatibility check with:

```sh
node integrations/htp/verify-contract.mjs /path/to/htp
```

This requires Cargo, Node.js and an HTP checkout with its PGlite dependency
installed in supabase/. It generates collector reports using the injected fixture
transport, runs HTP's receiver migrations in an isolated in-memory database, and
checks exact bytes, hash receipts/retries, metadata, summaries and owner isolation.
Complete, resource-limited and robots-excluded runs are covered. It never contacts
a live website or HTP endpoint; temporary reports and the database are discarded.
