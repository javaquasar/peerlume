import copy
import unittest

from measure_cold_models import validate_phase


class ColdModelContract(unittest.TestCase):
    def fixture(self,phase):
        buffers={"file_slots_bytes":0,"path_capacity_bytes":0,"tree_capacity_bytes":0,"view_capacity_bytes":0,"compact_capacity_bytes":0,"priority_capacity_bytes":0}
        if phase=="loaded":buffers["path_capacity_bytes"]=50
        if phase=="owner_cold":buffers["compact_capacity_bytes"]=16_000
        buffers["total_bytes"]=sum(buffers.values())
        return {"workload":"cold_model_phase_0_3_0","phase":phase,"identity":{"fixture":1},"files":1000,"selected_count":1000,"total_size":262_144_000,"selected_size":262_144_000,"selected_downloaded":0,"details_loaded":phase=="loaded","inline_model_bytes":256,"accounted_buffers":buffers}

    def test_all_three_real_representations_are_distinguished(self):
        for phase in ("loaded","owner_cold","summary_mirror"):
            validate_phase(self.fixture(phase),phase,1000,{"fixture":1})

    def test_a_deferred_cache_cannot_claim_empty_authoritative_totals(self):
        value=self.fixture("owner_cold")
        value["files"]=0
        with self.assertRaises(ValueError):validate_phase(value,"owner_cold",1000,{"fixture":1})

    def test_cold_paths_and_fake_zero_buffer_totals_are_rejected(self):
        for phase in ("owner_cold","summary_mirror"):
            value=copy.deepcopy(self.fixture(phase))
            value["accounted_buffers"]["path_capacity_bytes"]=1
            value["accounted_buffers"]["total_bytes"]+=1
            with self.assertRaises(ValueError):validate_phase(value,phase,1000,{"fixture":1})


if __name__=="__main__":
    unittest.main()
