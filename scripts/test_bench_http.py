#!/usr/bin/env python3
"""
Unit tests for scripts/bench_http.py helpers.
"""

import os
import struct
import sys
import tempfile
import unittest

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from bench_http import MemorySampler, compute_percentiles, read_process_rss_kb, read_sample_game_ids


class TestBenchHttp(unittest.TestCase):
    def test_compute_percentiles_empty(self):
        res = compute_percentiles([])
        self.assertEqual(res["p50_us"], 0.0)
        self.assertEqual(res["p95_us"], 0.0)
        self.assertEqual(res["p99_us"], 0.0)

    def test_compute_percentiles_calculation(self):
        times = [float(i) for i in range(1, 101)]
        res = compute_percentiles(times)
        self.assertEqual(res["min_us"], 1.0)
        self.assertEqual(res["max_us"], 100.0)
        self.assertEqual(res["p50_us"], 51.0)
        self.assertEqual(res["p95_us"], 96.0)
        self.assertEqual(res["p99_us"], 100.0)

    def test_read_process_rss(self):
        rss = read_process_rss_kb(os.getpid())
        self.assertGreater(rss, 0)

    def test_memory_sampler(self):
        import time

        sampler = MemorySampler(os.getpid(), sample_interval_s=0.005)
        sampler.start()
        time.sleep(0.02)
        peak = sampler.stop()
        self.assertGreater(peak, 0)

    def test_read_sample_game_ids(self):
        tmp = tempfile.mkdtemp()
        try:
            idx_file = os.path.join(tmp, "games.idx")
            with open(idx_file, "wb") as f:
                # 32-byte header
                f.write(b"\x00" * 32)
                # Entry 1: ID 1000
                f.write(struct.pack(">Q", 1000) + b"\x00" * 8)
                # Entry 2: ID 2000
                f.write(struct.pack(">Q", 2000) + b"\x00" * 8)

            ids = read_sample_game_ids(tmp, count=5)
            self.assertEqual(ids, [1000, 2000])
        finally:
            import shutil
            shutil.rmtree(tmp, ignore_errors=True)


if __name__ == "__main__":
    unittest.main()
