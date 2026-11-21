# Draft transport migration for S02/S03

The draft adds `raw_headers: Option<Vec<(String, String)>>` to the public `seosmith::fetch::Response` struct. This is a **Rust source-breaking API change**: every external `Transport` implementation that constructs `Response` with a struct literal must add the field. It does not add a field to `Page`, `Report`, or the delivered v1/v2 JSON contracts.

For an existing transport that cannot preserve individual relevant HTTP field occurrences, initialize `raw_headers: None`. That represents unavailable header evidence; it is not an empty header list and cannot support a passing absence conclusion. A transport able to retain all UTF-8 `X-Robots-Tag` and `Link` field values separately may use `Some(vec![(name, value), ...])`; use `Some(vec![])` only if the headers were collected successfully and none of those fields was present. Any relevant non-UTF-8 field must make the raw collection unavailable (`None`) under the current draft. Values and order reflect the transport's header API, not a guarantee of original wire byte order. Continue to populate the existing merged `headers` map for legacy consumers.

Example addition to a literal:

```rust
Response {
    status: 200,
    headers,
    raw_headers: None, // unavailable, rather than a successfully empty set
    body,
    ttfb_ms,
}
```

SEOsmith is currently 0.2.0. This public breaking change requires a 0.3.0 minor release under pre-1.0 SemVer, unless redesigned to avoid breaking existing struct literals. Do not publish it as a compatible 0.2.x release. The version has deliberately not been bumped in this draft: the package version is also `ENGINE_VERSION` in reports and policy comparison identity, so the integration owner must coordinate the version bump and cross-version fixtures as part of an actual release. Confirm all external callers and include this migration in release notes. This document is not production enablement or a frozen occurrence/wire contract.
