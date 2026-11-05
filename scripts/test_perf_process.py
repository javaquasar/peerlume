"""Resource sampler units and boundary behavior, without opening a user process."""

import unittest
import copy

from sample_perf_process import SampleSummary, proc_memory, verify_workload


class SamplerContract(unittest.TestCase):
    def test_linux_proc_memory_uses_kibibytes_and_missing_is_unavailable(self):
        self.assertEqual(proc_memory("Name:\tfixture\nVmRSS:\t1024 kB\nVmHWM:\t2048 kB\n"), (1_048_576, 2_097_152))
        self.assertEqual(proc_memory("Name:\tfixture\n"), (None, None))

    def test_sampling_reports_coverage_and_does_not_present_a_sample_as_an_exact_peak(self):
        summary = SampleSummary()
        summary.observe(5, 100, 200)
        summary.observe(15, None, None)
        summary.observe(25, 300, 500)
        summary.observe(30, 200, 500)
        report = summary.report(40, 10)
        self.assertEqual(report["successful_samples"], 3)
        self.assertEqual(report["sampled_rss_peak_bytes"], 300)
        self.assertEqual(report["os_high_water_mark_observed_bytes"], 500)
        self.assertEqual(report["sampled_rss_peak_offset_ns"], 25)
        self.assertEqual(report["max_successful_sample_gap_ns"], 20)
        self.assertEqual(report["unsampled_start_ns"], 5)
        self.assertEqual(report["unsampled_tail_ns"], 10)

    def test_no_successful_sample_is_not_zero_memory(self):
        report = SampleSummary().report(100, 10)
        self.assertEqual(report["successful_samples"], 0)
        self.assertIsNone(report["sampled_rss_peak_bytes"])
        self.assertIsNone(report["unsampled_start_ns"])
        self.assertIsNone(report["unsampled_tail_ns"])

    def test_complete_but_mismatched_workloads_are_rejected(self):
        report = {
            "completed": True, "workload": "synthetic_frontend_and_checkpoint", "source_label": "fixture",
            "cases": [{"torrents": count, "files_per_torrent": 100, "view": {"repetitions": 200}, "checkpoint_unchanged": {"repetitions": 20}, "checkpoint_changed": {"repetitions": 20}} for count in (1, 10, 100)],
            "large_model": {"files": 100_000, "cached_search": {"repetitions": 200}},
        }
        verify_workload(report, 200, "fixture")
        for field in ("files_per_torrent", "torrents"):
            changed = copy.deepcopy(report)
            changed["cases"][0][field] += 1
            with self.assertRaises(ValueError):
                verify_workload(changed, 200, "fixture")
        with self.assertRaises(ValueError):
            verify_workload(report, 201, "fixture")
        with self.assertRaises(ValueError):
            verify_workload(report, 200, "wrong source")


if __name__ == "__main__":
    unittest.main()
