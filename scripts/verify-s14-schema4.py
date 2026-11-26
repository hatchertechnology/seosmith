#!/usr/bin/env python3
"""Verify fixtures/contracts/s14-schema4-v1 against its manifest.

  python3 scripts/verify-s14-schema4.py
      manifest byte/sha256/shape checks (no receiver needed)
  python3 scripts/verify-s14-schema4.py --admit <path to the HTP receiver 'admit' example>
      also runs the receiver's admit on every fixture, requires every one to
      admit, and checks the recorded admit summary digest when present
  python3 scripts/verify-s14-schema4.py --admit <bin> --write-admit
      records validator_revision, summary_sha256 and summary_bytes in the manifest
"""
import argparse, hashlib, json, pathlib, re, subprocess, sys

ROOT = pathlib.Path(__file__).resolve().parent.parent
DIR = ROOT / "fixtures/contracts/s14-schema4-v1"
CAP = 4_952_756


def fail(msg):
    print(f"FAIL: {msg}", file=sys.stderr)
    sys.exit(1)


def admit(binary, path):
    p = subprocess.run([binary, str(path)], capture_output=True)
    err = p.stderr.decode()
    if p.returncode != 0:
        return None, err.strip().splitlines()[-1] if err.strip() else "rejected"
    m = re.search(r"validator_revision=(\S+) sha256=(sha256:[0-9a-f]{64}) bytes=(\d+)", err)
    if not m:
        fail(f"cannot parse admit stderr for {path.name}: {err!r}")
    if hashlib.sha256(p.stdout).hexdigest() != m.group(2).split(":")[1]:
        fail(f"{path.name}: admit stdout digest differs from its reported digest")
    if len(p.stdout) != int(m.group(3)):
        fail(f"{path.name}: admit stdout length differs from its reported bytes")
    return {"validator_revision": m.group(1), "summary_sha256": m.group(2),
            "summary_bytes": int(m.group(3))}, None


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--admit")
    ap.add_argument("--write-admit", action="store_true")
    args = ap.parse_args()
    manifest_path = DIR / "manifest.json"
    manifest = json.loads(manifest_path.read_text())
    if manifest["max_report_bytes"] != CAP:
        fail("max_report_bytes drifted")
    listed = {f["path"] for f in manifest["files"]}
    on_disk = {p.name for p in DIR.glob("*.json")} - {"manifest.json"}
    if listed != on_disk:
        fail(f"manifest and directory differ: {sorted(listed ^ on_disk)}")
    for entry in manifest["files"]:
        raw = (DIR / entry["path"]).read_bytes()
        if len(raw) != entry["bytes"] or len(raw) > CAP:
            fail(f"{entry['path']}: size {len(raw)} vs manifest {entry['bytes']} (cap {CAP})")
        if hashlib.sha256(raw).hexdigest() != entry["sha256"]:
            fail(f"{entry['path']}: sha256 differs from the manifest")
        if raw.endswith(b"\n") or raw[:1] != b"{":
            fail(f"{entry['path']}: not the exact compact wire bytes")
        doc = json.loads(raw)
        if doc["schema_version"] != 4:
            fail(f"{entry['path']}: schema_version {doc['schema_version']}")
        ec = doc["evidence_contract"]
        if ec["version"] != 1 or "finding_coverage" not in doc or "policy_guidance" not in doc:
            fail(f"{entry['path']}: missing evidence_contract v1, finding_coverage or policy_guidance")
        if len(ec["evaluations"]) != entry["evaluations"] or ec["truncated"] != entry["evidence_truncated"]:
            fail(f"{entry['path']}: evaluation count or truncated flag differs from the manifest")
        if len(doc["findings"]) != entry["findings"]:
            fail(f"{entry['path']}: finding count differs from the manifest")
        if entry["path"] == "exact-cap.json" and len(raw) != CAP:
            fail("exact-cap.json is not exactly the HTP cap")
        if entry["path"] == "crawl.json" and (entry["evidence_truncated"] or entry["evaluations"] > 460):
            fail("crawl.json violates the N-M4 bounds (evaluations <= 460, not truncated)")
    if args.admit:
        for entry in manifest["files"]:
            result, err = admit(args.admit, DIR / entry["path"])
            if result is None:
                fail(f"{entry['path']} rejected by admit: {err}")
            if args.write_admit:
                entry["admit"] = result
            elif entry.get("admit") not in (None, result):
                fail(f"{entry['path']}: admit result differs from the manifest")
            print(f"admit ok {entry['path']} {result['summary_sha256']}")
        if args.write_admit:
            manifest_path.write_text(json.dumps(manifest, indent=2) + "\n")
    print(f"ok: {len(manifest['files'])} fixtures verified")


main()
