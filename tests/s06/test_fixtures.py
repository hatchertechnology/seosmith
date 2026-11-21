"""Offline corpus checks only: deliberately no classifier or wire assertions."""
import hashlib
import json
from pathlib import Path
import unittest

ROOT = Path(__file__).resolve().parents[2] / 'fixtures/s06'


def unique_object(pairs):
    result = {}
    for key, value in pairs:
        if key in result:
            raise ValueError(f'duplicate label key: {key}')
        result[key] = value
    return result


class FixtureTest(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.labels = json.loads((ROOT / 'labels.json').read_bytes(), object_pairs_hook=unique_object)
        cls.cases = {case['id']: case for case in cls.labels['cases']}

    def test_retained_sources_and_identity(self):
        self.assertEqual(self.labels['version'], 1)
        self.assertEqual(self.labels['semantics'], 's06-fixture-design-1')
        self.assertEqual(len(self.cases), len(self.labels['cases']))
        self.assertEqual(len({c['url'] for c in self.cases.values()}), len(self.cases))
        referenced = set()
        for case in self.cases.values():
            with self.subTest(case=case['id']):
                self.assertTrue(case['url'].startswith('https://fixture.invalid/'))
                self.assertTrue(case['reason'])
                files = ([case['html']] if case['html'] else []) + case['related']
                for name in files:
                    self.assertEqual(Path(name).name, name)
                    self.assertTrue((ROOT / name).is_file())
                    referenced.add(name)
                if case['html']:
                    digest = hashlib.sha256((ROOT / case['html']).read_bytes()).hexdigest()
                    self.assertEqual(case['source_sha256'], digest)
                else:
                    self.assertIsNone(case['source_sha256'])
        self.assertEqual(referenced, {p.name for p in ROOT.glob('*.html')})

    def test_review_and_unavailable_boundaries(self):
        for case in self.cases.values():
            with self.subTest(case=case['id']):
                expected = case['expected']
                self.assertTrue(expected is None or type(expected) is bool)
                state = 'unknown' if expected is None else 'manual_review' if expected else 'no_candidate'
                self.assertEqual(case['review_state'], state)
                if case['response']['availability'] != 'available':
                    self.assertIsNone(expected)
        for name in ('empty-shell', 'loading-shell', 'unavailable-pair'):
            self.assertIsNone(self.cases[name]['expected'])
        for name in ('product-variants', 'useful-hours', 'useful-contact', 'error-phrase-guide', 'shared-template', 'real-not-found'):
            self.assertIs(self.cases[name]['expected'], False)
        for name in ('error-with-success', 'location-variants', 'exact-duplicate'):
            self.assertIs(self.cases[name]['expected'], True)

    def test_controls_isolate_status_and_duplicate_evidence(self):
        self.assertEqual((ROOT / 'error-200.html').read_bytes(), (ROOT / 'error-404.html').read_bytes())
        self.assertEqual(self.cases['error-with-success']['response']['status'], 200)
        self.assertEqual(self.cases['real-not-found']['response']['status'], 404)
        self.assertEqual((ROOT / 'service-east.html').read_bytes(), (ROOT / 'exact-copy.html').read_bytes())
        east = (ROOT / 'service-east.html').read_text()
        west = (ROOT / 'service-west.html').read_text()
        self.assertNotEqual(east, west)
        self.assertEqual(east.replace('Eastford', 'Westford'), west)
        self.assertNotEqual((ROOT / 'product-red.html').read_bytes(), (ROOT / 'product-blue.html').read_bytes())


if __name__ == '__main__':
    unittest.main()
