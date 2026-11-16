import json
import sys
import unittest
from pathlib import Path
sys.path.insert(0, str(Path(__file__).resolve().parents[2] / 'scripts/s15'))
from compare import scrapy_fixture

class ScrapyFixtureTest(unittest.TestCase):
    def test_feed(self):
        data = (Path(__file__).resolve().parents[2] / 'fixtures/s15/scrapy-2.13.4.json').read_bytes()
        lookup, _, _, _ = scrapy_fixture(data)
        self.assertTrue(lookup('https://fixture.invalid/missing-title', 'missing_title'))
        self.assertFalse(lookup('https://fixture.invalid/has-title', 'missing_title'))
        self.assertTrue(lookup('https://fixture.invalid/missing-description', 'missing_description'))
        self.assertFalse(lookup('https://fixture.invalid/has-description', 'missing_description'))
        self.assertIsNone(lookup('https://fixture.invalid/unavailable', 'missing_title'))
        self.assertIsNone(lookup('https://fixture.invalid/has-title', 'unmapped'))
        item = json.loads(data)[0]; item['file_status'] = 500
        self.assertIsNone(scrapy_fixture(json.dumps([item]))[0](item['url'], 'missing_title'))
        with self.assertRaises(ValueError): scrapy_fixture(json.dumps([item, item]))

    def test_ambiguous_and_malformed_feed(self):
        for duplicate in (
            '"title":null,"title":"ok"',
            '"profile":"s15-scrapy-fixture-v1","profile":"other"',
            '"url":"u","url":"v"',
            '"file_status":200,"file_status":500',
            '"extra":{"x":1,"x":2}',
        ):
            with self.subTest(duplicate=duplicate), self.assertRaises(ValueError):
                scrapy_fixture('[{"profile":"s15-scrapy-fixture-v1","url":"u",' + duplicate + '}]')
        for status in (200.0, True, "200", None):
            item = {"profile": "s15-scrapy-fixture-v1", "url": "u", "file_status": status, "title": "ok"}
            with self.subTest(status=status):
                self.assertIsNone(scrapy_fixture(json.dumps([item]))[0]("u", "missing_title"))
