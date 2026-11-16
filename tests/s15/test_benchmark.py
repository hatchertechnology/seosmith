import json
import os
from pathlib import Path
import subprocess
import sys
import unittest
import tempfile

ROOT = Path(__file__).resolve().parents[2]

class BenchmarkTest(unittest.TestCase):
    @unittest.skipUnless(os.environ.get('S15_BENCH_BINARY'), 'set S15_BENCH_BINARY to the compiled example')
    def test_labeled_crawler_report_is_scored_end_to_end(self):
        with tempfile.TemporaryDirectory() as directory:
            report = str(Path(directory) / 'report.json')
            command = [sys.executable, str(ROOT / 'scripts/s15/benchmark.py'), '--binary', os.environ['S15_BENCH_BINARY'], '--mode', 'labeled', '--report', report]
            run = subprocess.run(command, check=True, capture_output=True, text=True, timeout=20)
            self.assertTrue(json.loads(run.stdout)['coverage_complete'])
            compare = subprocess.run([sys.executable, str(ROOT / 'scripts/s15/compare.py'), '--labels', str(ROOT / 'fixtures/s15/labels.json'), '--export', report, '--adapter', 'seosmith-v1', '--assume-unmodified-v1'], check=True, capture_output=True, text=True)
            result = json.loads(compare.stdout)
            for rule in ('missing_title', 'missing_description'):
                self.assertEqual(result['rules'][rule]['tp'], 1)
                self.assertEqual(result['rules'][rule]['tn'], 1)
                self.assertEqual(result['rules'][rule]['unknown_observation'], 0)

    @unittest.skipUnless(os.environ.get('S15_BENCH_BINARY'), 'set S15_BENCH_BINARY to the compiled example')
    def test_controlled_budget_robots_and_deduplication(self):
        result = subprocess.run([sys.executable, str(ROOT / 'scripts/s15/benchmark.py'), '--binary', os.environ['S15_BENCH_BINARY']], check=True, capture_output=True, text=True, timeout=20)
        data = json.loads(result.stdout)
        self.assertEqual(data['fetch_attempt_count'], 4)
        self.assertEqual(data['frontier_unique_including_seed'], 5)
        self.assertEqual(data['completed_observations'], 3)
        self.assertEqual(data['stop_reason'], 'page_limit')
        self.assertFalse(data['coverage_complete'])
        self.assertNotIn('https://fixture.invalid/blocked', data['fetch_attempts'])
        self.assertEqual(sum(x['url'].endswith('/linked') for x in data['frontier']), 1)
        self.assertTrue(any(not x['observed'] for x in data['frontier']))
        self.assertGreater(data['peak_rss_kib_linux'], 0)

if __name__ == '__main__':
    unittest.main()
