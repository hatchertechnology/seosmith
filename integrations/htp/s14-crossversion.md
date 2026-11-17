# S14 cross-version rejection check

This is a pre-activation check against H01 at `d4dd085b8c94a657793d758857d57ddc4a17c7bd`, not schema 3 support. In an isolated HTP checkout at that revision, copy `fixtures/policies/v3-local-complete.json` from SEOsmith to `supabase/tests/fixtures/contracts/s14-v3.json`, apply `s14-crossversion-test.patch`, install the HTP Supabase test dependencies, and run:

```
node --test --test-name-pattern 'S14 generated schema 3' supabase/tests/seosmith-versioned.test.mjs
```

Both worker ingestion and owner import must reject the exact fixture and leave no audit row or receipt. The check passed locally on that H01 revision. Receiver schema 3 and H04 display support remain required before activation.
