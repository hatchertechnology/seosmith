# H15 controlled schema-2 producer pair

`pass.json` and `fail.json` are exact compact report bytes retrieved from `Store::report` after the actual SEOsmith crawl, v2 contract generation and persistence. No external network or production writes were used. The controlled `Transport` returns 404 for robots and sitemap and a single 200 HTML page, with or without a title. The generated reports use fixed website ID and binding generation 7; request IDs differ. Crawl observation timestamps were recorded at generation and are not per-fetch times.

Both records describe `https://example.com/`, entity `sha256:9ece6ef6e351f53de74785180edb5b1505ee396cdf9017e976d229e0e53b6a72`, `missing_title` rule version `2026-11-01.1`, validator `s01-1`, and policy signature `sha256:24f1711eb4c3ef600ac41b79f9b65f4b2bae2fa40f4d804fef40911c82d4e080`. The pass evaluation points to `/pages/0`; fail points to `/findings/8`. Both crawls are complete; overall per-rule evaluation coverage is not necessarily complete. They are controlled fixtures, not production evidence.

SHA-256 of exact committed bytes:

- `pass.json`: `a9f952a327d467fc5c632d6b4ae6799146bfaa323fae1c3624b9018a918c214f`
- `fail.json`: `4d504b4c6e0c8228ed8a8f253ae3eb90075bd70479932f1e17aec08413a5ed29`

Generate a fresh pair with `SEOSMITH_H15_PAIR_DIR=/tmp/seosmith-h15-pair-new cargo test --locked --test htp_surface_pair -- --ignored`. Fresh timestamps mean fresh exact-byte hashes; use the committed bytes for paired receiver tests. The test asserts the same entity, rule/validator version, policy signature, website/binding, and pass/fail outcomes. It makes no request to an HTP service.
