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
        self.assertEqual(self.labels['provenance'],
                         'Hand-authored synthetic source bytes and review labels; no crawler, rendered browser or provider capture.')
        self.assertEqual(set(self.cases), {
            'product-variants', 'useful-hours', 'useful-contact',
            'error-with-success', 'real-not-found', 'error-phrase-guide',
            'empty-shell', 'loading-shell', 'location-variants',
            'shared-template', 'exact-duplicate', 'fetch-failed',
            'robots-blocked', 'omitted-page', 'truncated-page',
            'missing-status', 'server-error', 'non-html', 'unavailable-pair',
        })
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
                self.assertEqual(set(case['related_sha256']), set(case['related']))
                for name in case['related']:
                    digest = hashlib.sha256((ROOT / name).read_bytes()).hexdigest()
                    self.assertEqual(case['related_sha256'][name], digest)
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

    def test_missing_sources_and_peer_remain_unavailable(self):
        for name, availability in {
            'fetch-failed': 'fetch_failed',
            'robots-blocked': 'robots_blocked',
            'omitted-page': 'not_collected',
            'non-html': 'non_html',
        }.items():
            with self.subTest(case=name):
                case = self.cases[name]
                self.assertIsNone(case['html'])
                self.assertIsNone(case['source_sha256'])
                self.assertIsNone(case['expected'])
                self.assertEqual(case['review_state'], 'unknown')
                self.assertEqual(case['response']['availability'], availability)
                self.assertEqual(case['related'], [])
        peer = self.cases['unavailable-pair']
        self.assertEqual(peer['unavailable_related'],
                         ['https://fixture.invalid/not-collected-peer'])
        self.assertEqual(peer['related'], [])
        self.assertIsNone(peer['expected'])
        self.assertEqual(peer['review_state'], 'unknown')
        self.assertNotIn(peer['unavailable_related'][0],
                         {case['url'] for case in self.cases.values()})

    def test_related_inventory_and_shared_template_control(self):
        peers = {
            'product-variants': ['product-blue.html'],
            'location-variants': ['service-west.html'],
            'shared-template': ['unique-service.html'],
            'exact-duplicate': ['exact-copy.html'],
        }
        for name, case in self.cases.items():
            self.assertEqual(case['related'], peers.get(name, []), name)
        # These fixed fixture landmarks isolate shared chrome from distinct main text.
        east = (ROOT / self.cases['shared-template']['html']).read_text()
        unique = (ROOT / peers['shared-template'][0]).read_text()
        def section(html, tag):
            return html.split(f'<{tag}>', 1)[1].split(f'</{tag}>', 1)[0]
        self.assertEqual(section(east, 'header'), section(unique, 'header'))
        self.assertEqual(section(east, 'footer'), section(unique, 'footer'))
        self.assertNotEqual(section(east, 'main'), section(unique, 'main'))

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
