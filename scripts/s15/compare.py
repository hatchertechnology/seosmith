#!/usr/bin/env python3
"""Compare explicitly labeled cases against bounded, retained offline exports."""
import argparse
import csv
import hashlib
import io
import json
from pathlib import Path

MAX_BYTES = 16 * 1024 * 1024
RULES = {"missing_title", "missing_description"}


def read_bounded(path):
    with open(path, "rb") as f:
        data = f.read(MAX_BYTES + 1)
    if len(data) > MAX_BYTES:
        raise ValueError("input exceeds 16 MiB")
    return data


def seosmith(data):
    report = json.loads(data)
    if report.get("schema_version") != 1:
        raise ValueError("unsupported SEOsmith schema version")
    pages, findings = report.get("pages"), report.get("findings")
    if not isinstance(pages, list) or not isinstance(findings, list):
        raise ValueError("missing pages or findings")
    page_by_url = {}
    for page in pages:
        url = page.get("url")
        if not isinstance(url, str) or url in page_by_url:
            raise ValueError("invalid or duplicate page URL")
        page_by_url[url] = page
    observed, unmapped, mapped = set(), [], []
    for finding in findings:
        if not isinstance(finding, dict) or not isinstance(finding.get("url"), str) or not isinstance(finding.get("rule"), str):
            raise ValueError("invalid finding")
        key = (finding["url"], finding["rule"])
        if finding["rule"] in RULES:
            observed.add(key)
            mapped.append(finding)
        else:
            unmapped.append(finding)
    complete = report.get("coverage_complete") is True and report.get("outcome") == "completed"
    def lookup(url, rule):
        page = page_by_url.get(url)
        if not complete or not page or page.get("error") or page.get("extraction_truncated") is not False:
            return None
        status, content_type = page.get("status"), page.get("content_type")
        if not isinstance(status, int) or not 200 <= status < 300 or not isinstance(content_type, str) or content_type.split(';')[0].strip().lower() not in ("text/html", "application/xhtml+xml", "application/vnd.wap.xhtml+xml"):
            return None
        return (url, rule) in observed
    return lookup, mapped, unmapped, {"schema_version": report["schema_version"], "engine_version": report.get("engine_version"), "ruleset_version": report.get("ruleset_version"), "outcome": report.get("outcome"), "stop_reason": report.get("stop_reason"), "coverage_complete": report.get("coverage_complete")}


def controlled_csv(data):
    # This is an interchange format for manually controlled exports, not a vendor adapter.
    reader = csv.DictReader(io.StringIO(data.decode("utf-8-sig"), newline=""))
    if reader.fieldnames != ["url", "rule", "state"]:
        raise ValueError("CSV columns must be url,rule,state")
    rows, mapped, unmapped = {}, [], []
    for row in reader:
        if None in row or not row["url"] or not row["rule"] or row["state"] not in ("present", "absent", "unknown"):
            raise ValueError("invalid CSV row")
        key = (row["url"], row["rule"])
        if key in rows:
            raise ValueError("duplicate CSV observation")
        rows[key] = {"present": True, "absent": False, "unknown": None}[row["state"]]
        if row["rule"] not in RULES:
            unmapped.append(row)
        else:
            mapped.append(row)
    return lambda url, rule: rows.get((url, rule)), mapped, unmapped, {"format": "controlled-csv-v1"}


def compare(labels, lookup):
    families = {}
    cases = []
    seen = set()
    for case in labels["cases"]:
        key = (case["url"], case["rule"])
        if key in seen or case["rule"] not in RULES or case["expected"] not in (True, False, None):
            raise ValueError("invalid or duplicate label")
        seen.add(key)
        family = families.setdefault(case["family"], {k: 0 for k in ("tp", "fp", "tn", "fn", "unknown_truth", "unknown_observation")})
        actual, expected = lookup(*key), case["expected"]
        if expected is None:
            family["unknown_truth"] += 1
        elif actual is None:
            family["unknown_observation"] += 1
        else:
            family["tp" if expected and actual else "fn" if expected else "fp" if actual else "tn"] += 1
        cases.append({"url": key[0], "rule": key[1], "expected": expected, "observed": actual})
    for counts in families.values():
        tp, fp, fn = counts["tp"], counts["fp"], counts["fn"]
        counts["precision"] = tp / (tp + fp) if tp + fp else None
        counts["recall"] = tp / (tp + fn) if tp + fn else None
    return families, cases


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--labels", required=True)
    parser.add_argument("--export", required=True)
    parser.add_argument("--adapter", required=True, choices=("seosmith-v1", "controlled-csv-v1"))
    args = parser.parse_args()
    labels_bytes, source = read_bounded(args.labels), read_bounded(args.export)
    labels = json.loads(labels_bytes)
    if labels.get("version") != 1 or labels.get("semantics") != "s15-metadata-1":
        raise ValueError("unsupported labels")
    lookup, mapped, unmapped, provenance = (seosmith if args.adapter == "seosmith-v1" else controlled_csv)(source)
    families, cases = compare(labels, lookup)
    print(json.dumps({"version": 1, "label_sha256": hashlib.sha256(labels_bytes).hexdigest(), "source_sha256": hashlib.sha256(source).hexdigest(), "adapter": args.adapter, "provenance": provenance, "families": families, "cases": cases, "mapped_evidence": mapped, "unmapped": unmapped}, indent=2))


if __name__ == "__main__":
    main()
