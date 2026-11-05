import tomllib
import unittest
from pathlib import Path

from measure_idle_owner import fixture_settings, proc_cpu_times


class IdleOwnerContracts(unittest.TestCase):
    def test_fixture_has_no_discovery_or_unsupported_binding_request(self):
        value = tomllib.loads(fixture_settings(Path("C:/synthetic profile")))
        self.assertEqual(value["download"]["download_directory"], str(Path("C:/synthetic profile") / "downloads"))
        self.assertFalse(any(value["bittorrent"][name] for name in ("dht", "pex", "lsd")))
        self.assertFalse(value["connection"]["port_forwarding"])
        self.assertFalse(value["connection"]["utp_enabled"])
        self.assertNotIn("bind_address", value["connection"])
        self.assertFalse(value["privacy"]["blocklist_auto_update"])

    def test_live_linux_cpu_fields_handle_spaces_and_parentheses_in_comm(self):
        fields = ["S"] + ["0"] * 10 + ["123", "45"] + ["0"] * 10
        self.assertEqual(proc_cpu_times("42 (synthetic ) owner) " + " ".join(fields), 100),
                         (1_230_000_000, 450_000_000))

    def test_incomplete_live_cpu_counters_are_not_treated_as_zero(self):
        with self.assertRaises(ValueError):
            proc_cpu_times("42 (short) S 1", 100)


if __name__ == "__main__":
    unittest.main()
