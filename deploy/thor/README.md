# Thor first installation

This is a single-site, single-replica sender. The v2 image currently on the node was built from SEOsmith merge `491bead59d35c01c1275527793fd539d7765a7e4` with locked dependencies and revision label; its imported manifest digest is `sha256:7073b6d33435d0a393dae26e6a25c34c4d2182f27870ef5372768d585a4822f1`; Docker archive SHA-256 is `68bd5c7502d077a489c0ebc43965814adda994b0c2a977344e758c87dd3d4ab4`, and image configuration ID is `sha256:57bc4201df35bfe5bfaeb67f2fd09190cf88b1fc1f5e57d53410278bfd13347c`. It is the 4a rollback candidate and stays on the node until step 4b has passed acceptance. The manifest pins the release binary (built from SEOsmith main `1a0b8708a2453ca78de8602cdc0c2d6637523782`, which contains S1 and E-p) by its imported manifest digest. `tests/deploy_thor.rs` fails closed on any placeholder or malformed image line, so nothing syncs from a commit that still carries one (see "Step 4a" below). Verify imported content and running image ID, retain the archive, and reimport by digest after node rebuild; this is not a remote-registry availability claim. The previous image manifest `sha256:cb2a47997d459a5e4c4b0dd9a11f64bbc4e60f0bc9d231cbe8e039534bd835fd` and Argo revision `f7b9e62772e8b271a109c1029be4d789352774cd` are rollback candidates subject to the compatibility procedure below. The `htp-prod` namespace already exists. Provision `/hatch1/services/seosmith` as a dedicated 10 GiB ZFS dataset owned by 10001:10001; the static PV and PVC retain state. Configure an independently generated API token and site-specific HTP token via the SOPS operator; neither belongs in Git plaintext. Provision the HTP binding only after confirming the intended site has none; do not rotate another binding. The service has no recurring scheduler. The production sender is scoped to the configured website binding. From step 4b it emits `s14-wire-1` schema-4 reports with S01 evidence v1: `SEOSMITH_POLICY_WIRE_V1=true`, `SEOSMITH_EVIDENCE_CONTRACT_V2=false`, `SEOSMITH_ADVISORY_POLICY=/etc/seosmith/policy.json` supplied by the generated `seosmith-policy` ConfigMap, and the `/integrations/seosmith/policy-audits` route. Step 4a, on the same binary, still emitted schema 2 (`SEOSMITH_EVIDENCE_CONTRACT_V2=true`, wire flag false, legacy `/audits` route, no advisory policy); that is the rollback direction. The binary refuses the wire flag together with the v2 evidence gate, and refuses the wire flag without an advisory policy.

The local execution-mode upgrade quarantines pre-upgrade queued or running jobs whose mode is unknown. Before upgrading, stop submissions and reconcile the queue; do not upgrade an uncertain delivery. Before rollback to an older binary, stop submissions and quiesce the sender, retain a verified backup, and check that no local-mode or ambiguous jobs can be claimed or counted incorrectly by the old binary. Do not delete or rewrite jobs to make a downgrade appear safe. If compatibility is uncertain, keep the sender stopped and recover forward. Preserve the compatible receiver and accepted report bytes.

The Argo Application uses manual sync. At bootstrap replace its `targetRevision: main` with the actual reviewed merge SHA before applying; later releases require an explicit reviewed revision change and sync. Before applying, verify receiver capability, node pressure, network policy enforcement, source/image identity, live owner/site and fresh database backup. Verify the ingress generated proxy labels and tailnet policy; test authenticated and denied traffic and HTP/DNS egress. Start with an empty queue and check startup, readiness and liveness. Deny public ingress, preserve auth and test only bounded approved targets. A Kubernetes manifest render does not by itself prove enforcement.

## Wire-2 and the three flag names

Three flags decide what the sender emits. Runbooks spell all three in full, because the shorter forms ("v1", "v2", "the v2 gate") mean different things:

| Flag | Meaning | Thor today |
|---|---|---|
| `SEOSMITH_EVIDENCE_CONTRACT_V2` | the older **schema-2** evidence gate (4a). It has nothing to do with wire-2, despite the `_V2` in its name. | `"false"` |
| `SEOSMITH_POLICY_WIRE_V1` | emit schema-4 `s14-wire-1` reports with S01 evidence v1 (4b). | `"true"` |
| `SEOSMITH_POLICY_WIRE_V2` | emit schema-4 `s14-wire-2` reports: `evidence_families`, digest manifest rows, S01 limits 1,024/1,024. See `docs/contracts/s14-wire-2.md`. | `"false"` |

`SEOSMITH_POLICY_WIRE_V2=true` requires `SEOSMITH_POLICY_WIRE_V1=true` and an advisory policy, and the binary refuses it together with `SEOSMITH_EVIDENCE_CONTRACT_V2=true` and on the local `crawl` subcommand. `tests/deploy_thor.rs` pins the three values and that combination rule in the committed manifest.

`SEOSMITH_POLICY_WIRE_V2` is stated explicitly as `"false"`. The image pinned in `resources.yaml` (`397ec342...`) was built before the flag existed and does not read it, so the line is inert until the wire-2 rollout (R6) pins a new image digest and flips the one value. The receiver must accept wire-2 first: a report sent to an older receiver is rejected terminally (`delivery_rejected`) and never stored.

Rolling back wire-2 is the one-word flip `SEOSMITH_POLICY_WIRE_V2` back to `"false"`: `SEOSMITH_POLICY_WIRE_V1` stays `"true"`, so, unlike rule 5 of the 4b rollback, the binary still starts and the next job is `s14-wire-1`. Wait until the queue is empty and no wire-2 job is pending before the receiver's wire-2 validator revision is disabled. Pinning the Argo revision back to the old image is safe only while no wire-2 rows exist, or after a purge: the old image cannot describe the new job error codes (`report_byte_limit`, `policy_manifest_limit`, ...).

### Writable `/tmp`

The pod keeps `readOnlyRootFilesystem: true`. A `tmp` `emptyDir` (`sizeLimit: 128Mi`) is mounted at `/tmp` because SQLite spills a sorted result to a temporary file once a row passes about 1.77 MB; with no writable temp directory `Store::pending` failed with `SQLITE_IOERR_GETTEMPPATH` (extended code 6410) and the whole delivery queue wedged behind one large report. Do not remove the volume or the mount; `tests/deploy_thor.rs` fails if either is missing. (The binary also sets `temp_store=MEMORY` and selects the pending row's id before its report, as `tests/pending_spill.rs` pins, so the mount is the second safeguard; an image that predates that fix still needs it.)

## Recovery and rollback

Before the first crawl and before changes: stop submissions; stop the only sender using a controlled manual GitOps action; ensure termination and no SQLite writer; archive the entire state directory including SQLite, WAL, SHM and report files, encrypt to independently recoverable age recipients, and copy to an off-host retained destination. Verify archive integrity and decrypt/restore into an isolated directory with no credentials or outbound delivery, then open SQLite and run `PRAGMA integrity_check`. Record artifact hashes, off-host location and result without tokens. Retain deployment source, exact image archive, SOPS-encrypted secret and keys separately. Snapshot on the same pool alone is not an off-host backup. Repeat after a crawl and on a documented schedule; restore testing of an empty initial database alone does not prove recovery of a report-bearing database.

First-install rollback is stop/scale-to-zero under manual GitOps control, and do not resync a one-replica manifest until recovery is approved and withdraw the private route if necessary; retain state, PV/PVC, SOPS secret, image and HTP audits. Reconcile every uncertain request before restarting. Before step 4a that is the whole rollback; from step 4a the sender image has a rollback candidate, and the rule is "4a rollback" below. Do not downgrade the receiver or rotate another site's binding. Never run restored queue state against production while testing recovery.

## Network-policy convergence gate

The init container delays sender startup and requires three successful public receiver health checks and failed connections to the injected Kubernetes API service IP and port, avoiding a DNS failure masquerading as denial. It has no service account token or app secrets. This reduces exposure to kube-router's asynchronous policy-programming window on pod creation and recreation, including when a queue exists. It does not rerun when only the application container restarts or detect subsequent policy loss. It does not prove all private routes are blocked: a Kubernetes API outage can look like denial, Service NAT and same-node traffic are implementation-dependent, and a successful public request does not prove the API service is available. Before rollout, independently verify the same API endpoint is reachable from an unselected control pod while it is denied from a policy-selected pod, check DNS and private ingress, and stop on discrepancies. Continue using the application's public-IP resolution and connection pinning. The pinned init image is Docker Hub `curlimages/curl:8.11.1` multi-architecture index `sha256:c1fe1679c34d9784c1b0d1e5f62ac0a79fca01fb6377cdd33e90473c6f9f9a69`; its linux/amd64 manifest is `sha256:ce8e655b562cf387752de7d2076ec3f81a4383c908f386b6724ec3571ce08b4a` and config is `sha256:7551dbeefe0d9a88a7134dd903f97727315e9fd150b4aefa6da5dcf3a4757f2a`. Import the index into the k3s containerd namespace and verify that exact digest resolves on Thor before sync; after node rebuild use the retained image archive or retrieve and verify this digest. The public root probe checks egress and TLS availability, not authenticated ingest readiness. No cluster-wide CNI settings are changed.

## Deployed state: scoped v2 activation (4a rollback reference)

"v2" in this section is the schema-2 evidence gate `SEOSMITH_EVIDENCE_CONTRACT_V2`, not wire-2 (`SEOSMITH_POLICY_WIRE_V2`; see "Wire-2 and the three flag names").

The one-site sender sets `SEOSMITH_EVIDENCE_CONTRACT_V2=true` for `serve`. Verify the receiver advertises schema 2 and evidence contract 1, the site binding and an empty or reconciled queue, then take and independently restore an encrypted off-host SQLite state backup before changing the pinned Argo revision. Use manual sync with the reviewed merge SHA and verify one bounded authorized crawl, exact receiver receipt and report bytes; keep older v1 reports readable. A v2 gate changes new processing and is not a migration of saved reports.

If rollout or delivery is ambiguous, stop submissions and scale to zero under manual Argo; reconcile the original request and receiver before restarting. Do not downgrade or replay unresolved reports. To roll back after reconciliation, use the retained prior reviewed `173d3e4de149a3466448fd799620399f803521be` Argo revision with v2 disabled, preserving the volume, encrypted secrets and all receiver rows; sync manually after checking queue compatibility. Verify the prior pinned images remain present. The restore test uses an isolated directory with delivery disabled, never the live volume.

## Step 4a: schema-4 binary, still schema 2

4a changes the image, plus one deliberate, behaviour-neutral line: `SEOSMITH_POLICY_WIRE_V1="false"` is now stated explicitly (the binary's default is false), so the 4b switch is a one-word diff. The resource class, volume, secret, network policy, init gate, `SEOSMITH_EVIDENCE_CONTRACT_V2=true`, the legacy `/integrations/seosmith/audits` endpoint and the absence of an advisory policy are unchanged. 4a admits no schema-4 row. The 4a commit that is synced is the one that carries the real image digest; step 4b is a separate commit reviewed after 4a has run.

The release artifact is the Docker archive built from SEOsmith main `1a0b8708a2453ca78de8602cdc0c2d6637523782` (unchanged Dockerfile: `rust:1.98.1-bookworm`, `cargo build --locked --release`, no Cargo features), image config ID `sha256:eed3ac5a2cbec4687452948506616f9013b4e587604d2c0ad8e35d6c0c3fb07d`, archive SHA-256 `36c994a9c0917d934c0a89770760939eca07443f9b7827ed15e65887082b2772`. It is not rebuilt: a rebuild runs `apt-get` and produces a different image.

Do these in order, and do not begin with the cluster.

1. Preconditions, from the rollout record: thor is scaled to zero, the queue is empty, and the encrypted off-host state archive has been restored and integrity-checked (see "Recovery and rollback"). The queue is empty when this returns 0, read with `mode=ro`:
   ```sql
   select count(*) from jobs
   where state in ('queued','running','local_queued','local_running')
      or (report is not null and delivery_state not in ('delivered','rejected'));
   ```
2. Image first. Import the archive into the k3s containerd namespace, record the manifest digest `<D>` that containerd reports, and verify identity:
   ```sh
   sha256sum seosmith-1a0b870.tar        # 36c994a9c0917d934c0a89770760939eca07443f9b7827ed15e65887082b2772
   sudo k3s ctr -n k8s.io images import --digests seosmith-1a0b870.tar
   sudo k3s crictl inspecti localhost/seosmith@sha256:<D>   # config id eed3ac5a2cbec4687452948506616f9013b4e587604d2c0ad8e35d6c0c3fb07d, revision label 1a0b8708a2453ca78de8602cdc0c2d6637523782, user 10001
   ```
   `<D>` is the value committed in `resources.yaml`: `397ec34200c6b01c6acc9684ac9d78e0824b770d97008960dce3390a44a94532`. The release build has no `local-e2e` feature, so it refuses `http://127.0.0.1` ingest URLs at startup.
3. Rehearsal, on the restored **isolated copy**, never the live volume. Run `thor-rehearse-4a.sh <isolated state copy> <new archive> <old archive>` from the rollout record, or its commands verbatim. Each image must be loaded into podman's own store first, because the k3s containerd copy is a different store:
   ```sh
   podman load -i <new archive>; podman load -i <old archive>
   podman run -d --name reh4a --network none \
     -v <isolated>/state:/data \
     -v <isolated>/secrets:/secrets:ro \
     -e SEOSMITH_DB=/data/seosmith.sqlite \
     -e SEOSMITH_EVIDENCE_CONTRACT_V2=true \
     -e SEOSMITH_API_TOKEN_FILE=/secrets/api-token \
     -e HTP_INGEST_TOKEN_FILE=/secrets/htp-token \
     -e HTP_INGEST_URL=https://127.0.0.1:9/integrations/seosmith/audits \
     <podman image> serve
   ```
   `--network none` leaves only loopback, so delivery cannot happen even with work queued, and `https://127.0.0.1:9` passes the startup HTTPS check yet refuses connections. The token files are a throwaway copy readable by uid 10001, and nothing is submitted (`POST /v1/runs` is never called). Without the two `_FILE` variables `serve` exits before it reports readiness. Run the NEW image first, then the retained v2 image `sha256:7073b6d33435…` on the same migrated copy. **A result counts only when the `Seosmith ready` log line appears on BOTH images** and each container stays up for another 60 s. Inside that netns readiness is that log line, logged right after bind; this deviates from plan 4a.1, which names `/health/ready`, because the host cannot reach a `--network none` container. The `/health/ready` route is exercised on the cluster in step 5.
   Record whether the **old** image opens the migrated copy. If it does not, 4a rollback is **recover forward only** and the manifest change must not be reverted.
4. Verify the render before the sync. `tests/deploy_thor.rs` is the fail-closed check (exactly one sender image line, matching `image: localhost/seosmith@sha256:` plus 64 hex characters; the init-gate curl image line likewise; no upper-case placeholder marker anywhere under `deploy/thor`):
   ```sh
   cargo test --test deploy_thor
   kubectl kustomize deploy/thor | grep -nE 'image:|SEOSMITH_|HTP_INGEST_URL'
   ```
   The second command must show the `<D>` digest, `SEOSMITH_EVIDENCE_CONTRACT_V2` `"true"`, `SEOSMITH_POLICY_WIRE_V1` `"false"`, and the legacy ingest URL.
5. Point the Argo Application at the reviewed merge SHA (a separate reviewed `targetRevision` change), sync manually, and scale to 1. Verify the init gate passed, the pod is Ready on `/health/ready`, the running image ID is `<D>`, and the log has no policy refusal.
6. Run one bounded **schema-2** crawl through `POST /v1/runs` for the bound site with `max_pages` 1. It must reach `delivery_state='delivered'` through the legacy route with schema 2 and evidence contract 1, and its receipt `sha256` must equal `jobs.report_hash`. That row joins binding generation 1, as the 21:49 lifecycle row did. Do not start 4b on a failed or ambiguous delivery.

Nothing in 4a deletes or rewrites a job. The same binary serves 4a and 4b; only the environment changes.

### 4a rollback

- Before any schema-4 row exists the only rollback is the binary. Keep the sender stopped and quiesced, and do not resync a one-replica manifest until the queue is reconciled.
- If the rehearsal showed the retained v2 image opens the migrated copy **and** `sudo k3s ctr images ls | grep 7073b6d33435` still resolves, revert to the recorded Argo revision `f7b9e62772e8b271a109c1029be4d789352774cd` (v2 image, `SEOSMITH_EVIDENCE_CONTRACT_V2=true`, legacy endpoint, no wire flag) and sync manually. `173d3e4de149a3466448fd799620399f803521be` is a v2-disabled state and is not a 4a rollback target.
- If the old image does not open the copy, or the rehearsal was not performed, **recover forward**: the sender stays stopped, a fresh encrypted off-host archive is taken, every job is left untouched, and the fix goes forward before resuming.
- Never delete jobs, downgrade the receiver, or rewrite saved report bytes to make a downgrade look safe.

## Step 4b: switch the sender to schema 4

The binary does not change in 4b; only the environment does. Prerequisites, each recorded elsewhere: HTP steps 1a, 1b, 2 and 3 are complete (schema-4 RPC and validator live, `HTP_S14_ADMISSION=true`, web build accepts schema 4), X has passed including its M8 payload measurement, and 4a's bounded schema-2 delivery was acknowledged with the exact receipt.

0. Database and series work, before any manifest change. Thor is stopped and the queue query returns 0. Run, from the HTP rollout record, first the reviewed guarded purge of the remaining schema-2 `seosmith_audits` rows for the bound site together with their derived lifecycle and cache rows, then 4b-0's guarded binding bump (expect generation 2, read back by a separate final `select`). Web history is scoped by website and FQDN rather than by generation, so a surviving schema-2 row would suppress new and reopened transitions for schema-4 rows; the generation bump is what keeps the generation-scoped alert processor's series clean. This repository ships neither payload: they are reviewed and run as HTP SQL, and nothing here deletes a thor job.
1. Verify the policy ConfigMap source before committing. `deploy/thor/policy.json` is a byte-identical copy of `fixtures/policies/production-default.json` (Q1: `{}`):
   ```sh
   cmp fixtures/policies/production-default.json deploy/thor/policy.json
   kubectl kustomize deploy/thor | sed -n '1,8p'
   ```
   The render must show one `ConfigMap` named `seosmith-policy-<hash>` with `policy.json: |` and `{}` under it, in namespace `htp-prod`. The hash suffix is why a policy change restarts the pod; if you change the policy, change the fixture, re-copy, re-commit and re-sync rather than editing the rendered object.
2. Preflight the whole switch in the render:
   ```sh
   kubectl kustomize deploy/thor | grep -nE 'image:|SEOSMITH_|HTP_INGEST_URL|mountPath: /etc/seosmith'
   ```
   Expected: the unchanged 4a digest; `SEOSMITH_EVIDENCE_CONTRACT_V2` `"false"`; `SEOSMITH_POLICY_WIRE_V1` `"true"`; `SEOSMITH_ADVISORY_POLICY` `/etc/seosmith/policy.json`; `HTP_INGEST_URL` ending `/integrations/seosmith/policy-audits`; and the read-only `/etc/seosmith` mount. Confirm the image digest still matches the one 4a verified, and run `cargo test --test deploy_thor`, which fails on any placeholder or malformed digest and checks the policy copy byte for byte.
3. Take a fresh state archive before the switch, using the five steps in "Recovery and rollback": archive the whole state directory including SQLite, WAL, SHM and report files; encrypt to the independent age recipients; copy off-host; decrypt and restore into an isolated directory with no credentials and no delivery; `PRAGMA integrity_check` must be `ok`. Record hashes and location. The pre-4a archive is not a substitute, because 4a's crawl wrote a new delivered row.
4. Point the Argo Application at the reviewed merge SHA for this commit if it changed, sync manually, and scale to 1. Verify the pod is Ready, the running image ID is still the 4a digest, `readOnlyRootFilesystem` did not block the policy mount, and the log has neither a policy parse error nor the "schema-4 wire carries its own evidence" refusal. Confirm the mounted bytes:
   ```sh
   kubectl -n htp-prod exec deploy/seosmith -- cat /etc/seosmith/policy.json
   kubectl -n htp-prod get cm | grep seosmith-policy
   ```
   The file must be `{}` and the ConfigMap name must be the hashed one the Deployment references.
   Argo CD renders with its own bundled kustomize, which may differ from the `kubectl kustomize` (kustomize v5.7.1) used in review. Before the sync record operator evidence that it renders the same: `kubectl -n argocd exec deploy/argocd-repo-server -- kustomize version`, or compare the Application's rendered manifests with the local render, in particular that the Deployment's `configMap` volume name carries the hash suffix and that the `namespace: htp-prod` line is honoured.
5. Hand over to the production acceptance check. Do not run further crawls until the acceptance record's bounds (`max_pages`, `max_resources`, evaluations, payload size) are the ones recorded by X; the first schema-4 delivery is measured for commit duration.

Delivery behaviour changes at this point: the `/policy-audits` route makes HTP `422` and `409` terminal (`delivery_state='rejected'`), while `400`, `5xx`, `429` and `503` still retry. A report that exceeds the byte cap or fails the generation checks ends the job `failed` with nothing delivered; the remedy is a lower `max_pages`, not a manifest change.

### 4b rollback

The first rule is unconditional: **never delete or rewrite thor jobs**. The second: if compatibility is uncertain, keep the sender stopped and recover forward.

1. Stop submissions and scale the sender to zero under manual Argo. Do not resync a one-replica manifest until the queue is reconciled.
2. Count schema-4 jobs twice (read-only, sender stopped; use the restored copy if the host has no read-only SQLite client). "Admitted" means a schema-4 report that HTP has accepted or terminally rejected, that is, any schema-4 row whose `delivery_state` is not `pending`; the unfiltered count covers it:
   ```sh
   sqlite3 -readonly /hatch1/services/seosmith/seosmith.sqlite \
     "select count(*) from jobs where report is not null and delivery_state='pending' and json_extract(report,'$.schema_version')=4"
   sqlite3 -readonly /hatch1/services/seosmith/seosmith.sqlite \
     "select count(*) from jobs where report is not null and json_extract(report,'$.schema_version')=4"
   ```
   The first counts undelivered schema-4 jobs; the second has no delivery filter, so it also counts `delivered` and terminally `rejected` rows (`422`/`409` on `/policy-audits`). `thor-steps.sh queue` prints both.
3. Only if BOTH counts are 0, bump the binding generation **again** with 4b-0's guarded payload so schema 2 and schema 4 never share a generation, then sync the step 4a commit (schema 2 on the same binary) and restore the 4a environment. Nothing is deleted; the schema-4 rows already delivered stay in HTP, and their lifecycle and alert series stay in the generation-2 series.
4. If either count is greater than 0, **quarantine**: the sender stays stopped, a fresh encrypted off-host archive is taken, every job is left untouched, the cause is fixed forward, and only then is the sender resumed. The same quarantine applies once at least one schema-4 row exists, because there is no downgrade path for a delivered schema-4 report.
5. Flipping `SEOSMITH_POLICY_WIRE_V1` back to `false` alone is NOT a rollback: with the evidence gate false and the advisory policy still mounted, the binary refuses to start (`S14 policy reports are delivered only with --policy-wire-v1`) and the pod crash-loops. The only working path is rule 3 (4a commit, 4a environment, after the generation bump); otherwise quarantine.
6. Never purge delivered HTP rows or the lifecycle, remediation and event-queue rows derived from them to make a downgrade look clean.
