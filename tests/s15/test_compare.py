import importlib.util
import json
from pathlib import Path
import unittest

ROOT = Path(__file__).resolve().parents[2]
spec = importlib.util.spec_from_file_location('compare', ROOT / 'scripts/s15/compare.py')
c = importlib.util.module_from_spec(spec)
spec.loader.exec_module(c)

class CompareTests(unittest.TestCase):
    def setUp(self):
        self.labels = json.loads((ROOT / 'fixtures/s15/labels.json').read_text())

    def test_controlled_labels_and_unmapped(self):
        lookup, mapped, unmapped, _ = c.controlled_csv((ROOT / 'fixtures/s15/controlled.csv').read_bytes())
        families, rules, _ = c.compare(self.labels, lookup)
        self.assertEqual(families['metadata'], {'tp': 2, 'tn': 2, 'fp': 0, 'fn': 0, 'unknown_truth': 1, 'unknown_observation': 0, 'precision': 1.0, 'recall': 1.0})
        self.assertEqual(len(unmapped), 1)
        self.assertEqual(len(mapped), 5)

    def test_partial_and_missing_page_are_unknown(self):
        page = {'url': 'https://fixture.invalid/has-title', 'status': 200, 'content_type': 'text/html', 'error': None, 'extraction_truncated': False}
        report = {'schema_version': 1, 'coverage_complete': False, 'outcome': 'partial', 'engine_version': '0.2.0', 'ruleset_version': '2026-11-01.1', 'pages': [page], 'findings': []}
        lookup, _, _, _ = c.seosmith(json.dumps(report).encode(), True)
        self.assertIsNone(lookup(page['url'], 'missing_title'))
        report.update(coverage_complete=True, outcome='completed')
        lookup, _, _, _ = c.seosmith(json.dumps(report).encode(), True)
        self.assertFalse(lookup(page['url'], 'missing_title'))
        self.assertIsNone(lookup('https://fixture.invalid/missing-title', 'missing_title'))
        self.assertEqual(c.compare(self.labels, lookup)[0]['metadata']['unknown_observation'], 3)

    def test_reject_duplicate_and_unknown_schema(self):
        with self.assertRaises(ValueError):
            c.controlled_csv(b'url,rule,state\na,missing_title,present\na,missing_title,absent\n')
        with self.assertRaises(ValueError):
            c.seosmith(b'{"schema_version":2}')

    def test_unknown_ruleset_and_unasserted_coverage(self):
        page = {'url': 'https://fixture.invalid/has-title', 'status': 200, 'content_type': 'text/html', 'error': None, 'extraction_truncated': False}
        report = {'schema_version': 1, 'coverage_complete': True, 'outcome': 'completed', 'engine_version': '0.2.0', 'ruleset_version': 'ancient', 'pages': [page], 'findings': []}
        lookup, _, _, _ = c.seosmith(json.dumps(report).encode(), True)
        self.assertIsNone(lookup(page['url'], 'missing_title'))
        report['ruleset_version'] = '2026-11-01.1'
        lookup, _, _, _ = c.seosmith(json.dumps(report).encode())
        self.assertIsNone(lookup(page['url'], 'missing_title'))

    def test_per_rule_detects_failure_hidden_by_family(self):
        _, rules, _ = c.compare(self.labels, lambda url, rule: rule == 'missing_title' and url.endswith('/missing-title'))
        self.assertEqual(rules['missing_title']['tp'], 1)
        self.assertEqual(rules['missing_description']['fn'], 1)
        with self.assertRaises(ValueError):
            c.compare({'cases': [{'url': 'a', 'rule': 'missing_title', 'family': 'metadata', 'expected': 1}]}, lambda *_: None)
        with self.assertRaises(ValueError):
            c.seosmith(b'{"schema_version":true}')

    def test_zero_denominators_are_null(self):
        families, rules, _ = c.compare(self.labels, lambda *_: None)
        self.assertIsNone(families['metadata']['precision'])
        self.assertIsNone(families['metadata']['recall'])

if __name__ == '__main__':
    unittest.main()
