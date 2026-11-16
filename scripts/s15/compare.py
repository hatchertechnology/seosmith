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


def seosmith(data, assume_unmodified_v1=False):
    report = json.loads(data)
    if not isinstance(report, dict) or type(report.get("schema_version")) is not int or report["schema_version"] != 1:
        raise ValueError("unsupported SEOsmith schema version")
    pages, findings = report.get("pages"), report.get("findings")
    if not isinstance(pages, list) or not isinstance(findings, list):
        raise ValueError("missing pages or findings")
    page_by_url = {}
    for page in pages:
        if not isinstance(page, dict):
            raise ValueError("invalid page")
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
    compatible = assume_unmodified_v1 and report.get("engine_version") == "0.2.0" and report.get("ruleset_version") == "2026-11-01.1"
    complete = compatible and report.get("coverage_complete") is True and report.get("outcome") == "completed"
    def lookup(url, rule):
        page = page_by_url.get(url)
        if not complete or not page or page.get("error") or page.get("extraction_truncated") is not False:
            return None
        status, content_type = page.get("status"), page.get("content_type")
        if type(status) is not int or not 200 <= status < 300 or not isinstance(content_type, str) or content_type.split(';')[0].strip().lower() not in ("text/html", "application/xhtml+xml", "application/vnd.wap.xhtml+xml"):
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


def scrapy_fixture(data):
    def unique_object(pairs):
        result = {}
        for key, value in pairs:
            if key in result:
                raise ValueError("duplicate JSON object key")
            result[key] = value
        return result

    items = json.loads(data, object_pairs_hook=unique_object)
    if not isinstance(items, list):
        raise ValueError("invalid Scrapy feed")
    rows = {}
    for item in items:
        if not isinstance(item, dict) or item.get("profile") != "s15-scrapy-fixture-v1" or not isinstance(item.get("url"), str) or item["url"] in rows:
            raise ValueError("invalid or duplicate fixture item")
        rows[item["url"]] = item
    def lookup(url, rule):
        item = rows.get(url)
        field = {"missing_title": "title", "missing_description": "description"}.get(rule)
        if not item or not field or type(item.get("file_status")) is not int or item["file_status"] != 200 or field not in item:
            return None
        value = item[field]
        if value is not None and not isinstance(value, str):
            return None
        return not bool(value and value.strip())
    return lookup, items, [], {"profile": "s15-scrapy-fixture-v1", "producer": "custom Scrapy fixture profile; version not verified by feed"}


def compare(labels, lookup):
    families, rules, cases, seen = {}, {}, [], set()
    if not isinstance(labels, dict) or not isinstance(labels.get("cases"), list):
        raise ValueError("invalid labels")
    for case in labels["cases"]:
        if not isinstance(case, dict) or not isinstance(case.get("url"), str) or not case["url"] or not isinstance(case.get("family"), str) or not case["family"] or case.get("rule") not in RULES or (case.get("expected") is not None and type(case.get("expected")) is not bool) or "expected" not in case:
            raise ValueError("invalid label")
        key = (case["url"], case["rule"])
        if key in seen:
            raise ValueError("duplicate label")
        seen.add(key)
        actual, expected = lookup(*key), case["expected"]
        bucket = "unknown_truth" if expected is None else "unknown_observation" if actual is None else "tp" if expected and actual else "fn" if expected else "fp" if actual else "tn"
        for group, name in ((families, case["family"]), (rules, case["rule"])):
            counts = group.setdefault(name, {k: 0 for k in ("tp", "fp", "tn", "fn", "unknown_truth", "unknown_observation")})
            counts[bucket] += 1
        cases.append({"url": key[0], "rule": key[1], "expected": expected, "observed": actual})
    for counts in list(families.values()) + list(rules.values()):
        tp, fp, fn = counts["tp"], counts["fp"], counts["fn"]
        counts["precision"] = tp / (tp + fp) if tp + fp else None
        counts["recall"] = tp / (tp + fn) if tp + fn else None
    return families, rules, cases


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--labels", required=True)
    parser.add_argument("--export", required=True)
    parser.add_argument("--adapter", required=True, choices=("seosmith-v1", "controlled-csv-v1", "scrapy-fixture-v1"))
    parser.add_argument("--assume-unmodified-v1", action="store_true", help="assert the controlled producer uses the unmodified known v1 ruleset")
    args = parser.parse_args()
    labels_bytes, source = read_bounded(args.labels), read_bounded(args.export)
    labels = json.loads(labels_bytes)
    if type(labels.get("version")) is not int or labels.get("version") != 1 or labels.get("semantics") != "s15-metadata-1":
        raise ValueError("unsupported labels")
    lookup, mapped, unmapped, provenance = seosmith(source, args.assume_unmodified_v1) if args.adapter == "seosmith-v1" else scrapy_fixture(source) if args.adapter == "scrapy-fixture-v1" else controlled_csv(source)
    provenance["unmodified_v1_asserted"] = args.assume_unmodified_v1 if args.adapter == "seosmith-v1" else None
    families, rules, cases = compare(labels, lookup)
    print(json.dumps({"version": 1, "label_sha256": hashlib.sha256(labels_bytes).hexdigest(), "source_sha256": hashlib.sha256(source).hexdigest(), "adapter": args.adapter, "provenance": provenance, "families": families, "rules": rules, "cases": cases, "mapped_evidence": mapped, "unmapped": unmapped}, indent=2))


if __name__ == "__main__":
    main()
