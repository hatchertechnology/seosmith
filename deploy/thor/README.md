# Thor first installation

This is a single-site, single-replica sender. The node-local image is built from SEOsmith merge `491bead59d35c01c1275527793fd539d7765a7e4` with locked dependencies and revision label. The v2 image currently on the node is imported manifest digest `sha256:7073b6d33435d0a393dae26e6a25c34c4d2182f27870ef5372768d585a4822f1`; Docker archive SHA-256 is `68bd5c7502d077a489c0ebc43965814adda994b0c2a977344e758c87dd3d4ab4`, and image configuration ID is `sha256:57bc4201df35bfe5bfaeb67f2fd09190cf88b1fc1f5e57d53410278bfd13347c`. It is the 4a rollback candidate and stays on the node until step 4b has passed acceptance. The manifest pins the merged S1+E-p binary by digest and currently carries `PLACEHOLDER-FILL-FROM-4A-RELEASE-BUILD`: that placeholder is replaced from the release build in a reviewed commit before the 4a sync (see "Step 4a" below). Verify imported content and running image ID, retain the archive, and reimport by digest after node rebuild; this is not a remote-registry availability claim. The previous image manifest `sha256:cb2a47997d459a5e4c4b0dd9a11f64bbc4e60f0bc9d231cbe8e039534bd835fd` and Argo revision `f7b9e62772e8b271a109c1029be4d789352774cd` are rollback candidates subject to the compatibility procedure below. The `htp-prod` namespace already exists. Provision `/hatch1/services/seosmith` as a dedicated 10 GiB ZFS dataset owned by 10001:10001; the static PV and PVC retain state. Configure an independently generated API token and site-specific HTP token via the SOPS operator; neither belongs in Git plaintext. Provision the HTP binding only after confirming the intended site has none; do not rotate another binding. The service has no recurring scheduler. The production sender is scoped to the configured website binding and, at step 4a, still emits S01 schema-v2 evidence: `SEOSMITH_EVIDENCE_CONTRACT_V2=true`, `SEOSMITH_POLICY_WIRE_V1=false`, the legacy `/integrations/seosmith/audits` route and no advisory policy. Do not set an advisory policy or the wire flag before step 4b; the binary refuses that combination at startup.

The local execution-mode upgrade quarantines pre-upgrade queued or running jobs whose mode is unknown. Before upgrading, stop submissions and reconcile the queue; do not upgrade an uncertain delivery. Before rollback to an older binary, stop submissions and quiesce the sender, retain a verified backup, and check that no local-mode or ambiguous jobs can be claimed or counted incorrectly by the old binary. Do not delete or rewrite jobs to make a downgrade appear safe. If compatibility is uncertain, keep the sender stopped and recover forward. Preserve the compatible receiver and accepted report bytes.

The Argo Application uses manual sync. At bootstrap replace its `targetRevision: main` with the actual reviewed merge SHA before applying; later releases require an explicit reviewed revision change and sync. Before applying, verify receiver capability, node pressure, network policy enforcement, source/image identity, live owner/site and fresh database backup. Verify the ingress generated proxy labels and tailnet policy; test authenticated and denied traffic and HTP/DNS egress. Start with an empty queue and check startup, readiness and liveness. Deny public ingress, preserve auth and test only bounded approved targets. A Kubernetes manifest render does not by itself prove enforcement.

## Recovery and rollback

Before the first crawl and before changes: stop submissions; stop the only sender using a controlled manual GitOps action; ensure termination and no SQLite writer; archive the entire state directory including SQLite, WAL, SHM and report files, encrypt to independently recoverable age recipients, and copy to an off-host retained destination. Verify archive integrity and decrypt/restore into an isolated directory with no credentials or outbound delivery, then open SQLite and run `PRAGMA integrity_check`. Record artifact hashes, off-host location and result without tokens. Retain deployment source, exact image archive, SOPS-encrypted secret and keys separately. Snapshot on the same pool alone is not an off-host backup. Repeat after a crawl and on a documented schedule; restore testing of an empty initial database alone does not prove recovery of a report-bearing database.

First-install rollback is stop/scale-to-zero under manual GitOps control, and do not resync a one-replica manifest until recovery is approved and withdraw the private route if necessary; retain state, PV/PVC, SOPS secret, image and HTP audits. Reconcile every uncertain request before restarting. There is no prior sender release to roll back to. Do not downgrade the receiver or rotate another site's binding. Never run restored queue state against production while testing recovery.

## Network-policy convergence gate

The init container delays sender startup and requires three successful public receiver health checks and failed connections to the injected Kubernetes API service IP and port, avoiding a DNS failure masquerading as denial. It has no service account token or app secrets. This reduces exposure to kube-router's asynchronous policy-programming window on pod creation and recreation, including when a queue exists. It does not rerun when only the application container restarts or detect subsequent policy loss. It does not prove all private routes are blocked: a Kubernetes API outage can look like denial, Service NAT and same-node traffic are implementation-dependent, and a successful public request does not prove the API service is available. Before rollout, independently verify the same API endpoint is reachable from an unselected control pod while it is denied from a policy-selected pod, check DNS and private ingress, and stop on discrepancies. Continue using the application's public-IP resolution and connection pinning. The pinned init image is Docker Hub `curlimages/curl:8.11.1` multi-architecture index `sha256:c1fe1679c34d9784c1b0d1e5f62ac0a79fca01fb6377cdd33e90473c6f9f9a69`; its linux/amd64 manifest is `sha256:ce8e655b562cf387752de7d2076ec3f81a4383c908f386b6724ec3571ce08b4a` and config is `sha256:7551dbeefe0d9a88a7134dd903f97727315e9fd150b4aefa6da5dcf3a4757f2a`. Import the index into the k3s containerd namespace and verify that exact digest resolves on Thor before sync; after node rebuild use the retained image archive or retrieve and verify this digest. The public root probe checks egress and TLS availability, not authenticated ingest readiness. No cluster-wide CNI settings are changed.

## Deployed state: scoped v2 activation (4a rollback reference)

The one-site sender sets `SEOSMITH_EVIDENCE_CONTRACT_V2=true` for `serve`. Verify the receiver advertises schema 2 and evidence contract 1, the site binding and an empty or reconciled queue, then take and independently restore an encrypted off-host SQLite state backup before changing the pinned Argo revision. Use manual sync with the reviewed merge SHA and verify one bounded authorized crawl, exact receiver receipt and report bytes; keep older v1 reports readable. A v2 gate changes new processing and is not a migration of saved reports.

If rollout or delivery is ambiguous, stop submissions and scale to zero under manual Argo; reconcile the original request and receiver before restarting. Do not downgrade or replay unresolved reports. To roll back after reconciliation, use the retained prior reviewed `173d3e4de149a3466448fd799620399f803521be` Argo revision with v2 disabled, preserving the volume, encrypted secrets and all receiver rows; sync manually after checking queue compatibility. Verify the prior pinned images remain present. The restore test uses an isolated directory with delivery disabled, never the live volume.

## Step 4a: schema-4 binary, still schema 2

4a changes exactly one thing: the image. The resource class, volume, secret, network policy, init gate, `SEOSMITH_EVIDENCE_CONTRACT_V2=true`, the legacy `/integrations/seosmith/audits` endpoint and the absence of an advisory policy are unchanged, and `SEOSMITH_POLICY_WIRE_V1=false` is now stated explicitly so the 4b switch is a one-word diff. 4a admits no schema-4 row. This commit is reviewed on its own; step 4b is a separate commit reviewed after 4a has run.

Do these in order, and do not begin with the cluster.

1. Preconditions, from the rollout record: thor is scaled to zero, the queue query returns 0, and the encrypted off-host state archive has been restored and integrity-checked (see "Recovery and rollback").
2. Rehearsal, on the restored **isolated copy**, never the live volume. Start the new image with delivery impossible, then the retained v2 image `sha256:7073b6d33435…` on the same migrated copy:
   ```sh
   cp -a <restore>/state <isolated>/state
   podman run --rm --network none \
     -v <isolated>/state:/data \
     -v <isolated>/secrets:/secrets:ro \
     -e SEOSMITH_DB=/data/seosmith.sqlite \
     -e SEOSMITH_EVIDENCE_CONTRACT_V2=true \
     -e HTP_INGEST_URL=https://127.0.0.1:9/integrations/seosmith/audits \
     <image-digest> serve
   ```
   `--network none` leaves only loopback, so delivery cannot happen even with work queued, and `https://127.0.0.1:9` passes the startup HTTPS check yet refuses connections. The token files are a throwaway copy readable by uid 10001, and nothing is submitted (`POST /v1/runs` is never called). Inside that netns readiness is the `Seosmith ready` line logged right after bind; the `/health/ready` route is exercised in step 4 on the cluster instead. Let each image run 60 s, then stop it.
   Record whether the **old** image opens the migrated copy. If it does not, 4a rollback is **recover forward only** and the manifest change must not be reverted.
3. Build the image at the merged S1+E-p SHA with the unchanged Dockerfile (`cargo build --release --locked`, no Cargo features), keep the Docker archive, and import it into the k3s containerd namespace:
   ```sh
   sudo k3s ctr -n k8s.io images import <archive>
   sudo k3s ctr images ls | grep seosmith
   ```
   Verify the imported manifest digest equals the build output, and that this binary still refuses `http://127.0.0.1` at startup: the test-only `local-e2e` feature is absent from the release build.
4. Replace `PLACEHOLDER-FILL-FROM-4A-RELEASE-BUILD` in `resources.yaml` with the verified digest and commit. Preflight the render both ways:
   ```sh
   kubectl kustomize deploy/thor | grep -n PLACEHOLDER        # must print nothing
   kubectl kustomize deploy/thor | grep -nE 'image:|SEOSMITH_|HTP_INGEST_URL'
   ```
   The second command must show the new digest, `SEOSMITH_EVIDENCE_CONTRACT_V2` `"true"`, `SEOSMITH_POLICY_WIRE_V1` `"false"`, and the legacy ingest URL.
5. Point the Argo Application at the reviewed merge SHA (a separate reviewed `targetRevision` change), sync manually, and scale to 1. Verify the init gate passed, the pod is Ready, the running image ID is the new digest, and the log has no policy refusal.
6. Run one bounded **schema-2** crawl through `POST /v1/runs` for the bound site with `max_pages` 1. It must reach `delivery_state='delivered'` through the legacy route with schema 2 and evidence contract 1, and its receipt `sha256` must equal `jobs.report_hash`. That row joins binding generation 1, as the 21:49 lifecycle row did. Do not start 4b on a failed or ambiguous delivery.

Nothing in 4a deletes or rewrites a job. The same binary serves 4a and 4b; only the environment changes.

### 4a rollback

- Before any schema-4 row exists the only rollback is the binary. Keep the sender stopped and quiesced, and do not resync a one-replica manifest until the queue is reconciled.
- If the rehearsal showed the retained v2 image opens the migrated copy **and** `sudo k3s ctr images ls | grep 7073b6d33435` still resolves, revert to the recorded Argo revision (v2 image, `SEOSMITH_EVIDENCE_CONTRACT_V2=true`, legacy endpoint, no wire flag) and sync manually.
- If the old image does not open the copy, or the rehearsal was not performed, **recover forward**: the sender stays stopped, a fresh encrypted off-host archive is taken, every job is left untouched, and the fix goes forward before resuming.
- Never delete jobs, downgrade the receiver, or rewrite saved report bytes to make a downgrade look safe.
