#!/usr/bin/env python3
"""Verify fixtures/contracts/s14-schema4-v1 against its manifest.

  python3 scripts/verify-s14-schema4.py
      manifest byte/sha256/shape checks (no receiver needed)
  python3 scripts/verify-s14-schema4.py --admit <path to the HTP receiver 'admit' example>
      also runs the receiver's admit on every fixture, requires every one to
      admit, and checks the recorded admit summary digest when present
  python3 scripts/verify-s14-schema4.py --admit <bin> --write-admit
      records validator_revision, summary_sha256 and summary_bytes in the manifest,
      plus the receiver commit given by --receiver-commit <sha>

s14-wire-2 fixtures (fixtures/wire2/, see docs/contracts/s14-wire-2.md):

  python3 scripts/verify-s14-schema4.py --wire2
      manifest byte/sha256/shape checks (no receiver needed)
  python3 scripts/verify-s14-schema4.py --wire2 --admit <path to the HTP receiver 'admit' example>
      also runs the receiver on every file and compares the outcome with the manifest's
      `expect`: an admitted file must report the expected validator revision; a rejected
      file must end its stderr with exactly one `rejected stage=<inspection|summary|population>
      error=<InspectionError name>` line (design 5 stage contract) equal to the expected
      stage and error. Any difference fails.
  ... --wire2 --admit <bin> --write-admit --receiver-commit <sha>
      records each file's observed outcome under `admit` and the receiver commit
"""
import argparse, hashlib, json, pathlib, re, subprocess, sys

ROOT = pathlib.Path(__file__).resolve().parent.parent
DIR = ROOT / "fixtures/contracts/s14-schema4-v1"
DIR2 = ROOT / "fixtures/wire2"
CAP = 4_952_756
V1_REVISION = "s14-wire-1/full-population-1/evidence-v1"
V2_REVISION = "s14-wire-2/full-population-1/evidence-v1/families-v1"
STAGES = ("inspection", "summary", "population")


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


REJECT_LINE = re.compile(r"rejected stage=(\S+) error=(\S+)")


def admit2(binary, path):
    """Run the receiver on one wire-2 fixture; return its observed outcome."""
    p = subprocess.run([binary, str(path)], capture_output=True)
    lines = [l for l in p.stderr.decode().splitlines() if l.strip()]
    last = lines[-1] if lines else ""
    if p.returncode != 0:
        if p.returncode != 1:
            fail(f"{path.name}: receiver exited {p.returncode}, a rejection exits 1")
        if sum(1 for l in lines if l.startswith("rejected stage=")) != 1:
            fail(f"{path.name}: expected exactly one `rejected stage=` line: {p.stderr.decode()!r}")
        m = REJECT_LINE.fullmatch(last)
        if not m:
            fail(f"{path.name}: rejected without a `rejected stage=.. error=..` last line: {last!r}")
        if m.group(1) not in STAGES:
            fail(f"{path.name}: unknown stage {m.group(1)!r}")
        return {"admit": False, "stage": m.group(1), "error": m.group(2)}
    m = re.search(r"validator_revision=(\S+) sha256=(sha256:[0-9a-f]{64}) bytes=(\d+)", p.stderr.decode())
    if not m:
        fail(f"{path.name}: cannot parse the admit line: {p.stderr.decode()!r}")
    return {"admit": True, "validator_revision": m.group(1), "summary_sha256": m.group(2),
            "summary_bytes": int(m.group(3))}


def verify_wire2(args):
    if args.write_admit and not args.admit:
        fail("--write-admit requires --admit")
    manifest_path = DIR2 / "manifest.json"
    manifest = json.loads(manifest_path.read_text())
    if manifest["max_report_bytes"] != CAP:
        fail("max_report_bytes drifted")
    if manifest["v1_revision"] != V1_REVISION or manifest["v2_revision"] != V2_REVISION:
        fail("validator revisions drifted")
    listed = {f["path"] for f in manifest["files"]}
    on_disk = {f"{sub}/{p.name}" for sub in ("positive", "negative") for p in (DIR2 / sub).glob("*.json")}
    if listed != on_disk:
        fail(f"manifest and directories differ: {sorted(listed ^ on_disk)}")
    for entry in manifest["files"]:
        path = entry["path"]
        raw = (DIR2 / path).read_bytes()
        if len(raw) != entry["bytes"]:
            fail(f"{path}: size {len(raw)} vs manifest {entry['bytes']}")
        if hashlib.sha256(raw).hexdigest() != entry["sha256"]:
            fail(f"{path}: sha256 differs from the manifest")
        if raw.endswith(b"\n") or raw[:1] != b"{":
            fail(f"{path}: not the exact compact wire bytes")
        e = entry["expect"]
        positive = path.startswith("positive/")
        if positive:
            if len(raw) > CAP:
                fail(f"{path}: positive over the cap")
            if e != {"admit": True, "validator_revision": V2_REVISION}:
                fail(f"{path}: a positive must expect admit with the v2 revision")
        else:
            if e.get("admit") is not False or e.get("stage") not in STAGES or not e.get("error"):
                fail(f"{path}: a negative must expect a rejection at inspection, summary or population")
        if path == "positive/exact-cap-v2.json" and len(raw) != CAP:
            fail("exact-cap-v2.json is not exactly the HTP cap")
        if positive:
            doc = json.loads(raw)
            if doc["schema_version"] != 4 or doc["policy_guidance"]["version"] != "s14-wire-2":
                fail(f"{path}: not a schema-4 s14-wire-2 report")
            if "evidence_families" not in doc:
                fail(f"{path}: no evidence_families")
    if args.admit:
        for entry in manifest["files"]:
            got = admit2(args.admit, DIR2 / entry["path"])
            e = entry["expect"]
            if got["admit"] != e["admit"]:
                fail(f"{entry['path']}: receiver {'admitted' if got['admit'] else 'rejected'} it "
                     f"({got}), manifest expects {e}")
            if got["admit"]:
                if got["validator_revision"] != e["validator_revision"]:
                    fail(f"{entry['path']}: validator_revision {got['validator_revision']} vs {e['validator_revision']}")
            elif (got["stage"], got["error"]) != (e["stage"], e["error"]):
                fail(f"{entry['path']}: receiver says stage={got['stage']} error={got['error']}, "
                     f"manifest expects stage={e['stage']} error={e['error']}")
            if args.write_admit:
                entry["admit"] = got
            print(f"admit parity {entry['path']} " + (got["validator_revision"] if got["admit"]
                  else f"{got['stage']}/{got['error']}"))
        if args.write_admit:
            if not args.receiver_commit:
                fail("--write-admit requires --receiver-commit")
            manifest["admit_receiver_commit"] = args.receiver_commit
            manifest_path.write_text(json.dumps(manifest, indent=2) + "\n")
    print(f"ok: {len(manifest['files'])} wire2 fixtures verified")


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--wire2", action="store_true", help="verify fixtures/wire2 instead of the schema-4 v1 set")
    ap.add_argument("--admit")
    ap.add_argument("--write-admit", action="store_true")
    ap.add_argument("--receiver-commit", help="HTP commit the admit binary was built from; recorded with --write-admit")
    args = ap.parse_args()
    if args.wire2:
        return verify_wire2(args)
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
            if not args.receiver_commit:
                fail("--write-admit requires --receiver-commit")
            manifest["admit_receiver_commit"] = args.receiver_commit
            manifest_path.write_text(json.dumps(manifest, indent=2) + "\n")
    print(f"ok: {len(manifest['files'])} fixtures verified")


if __name__ == "__main__":
    main()
