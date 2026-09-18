#!/usr/bin/env python3
import unittest
from workload import parse_workload, schedule_provenance


class WorkloadTests(unittest.TestCase):
    def test_defaults_and_schedule_manifest(self):
        default = parse_workload("5", "0.5", "0.05", "", 120)
        self.assertEqual((default["band"], default["cross_fraction"], default["cancel_fraction"]), (5, .5, .05))
        self.assertEqual(default["rate_schedule"], [])
        self.assertFalse(default["rate_total_overridden"])
        custom = parse_workload("1", ".2", "0", "0:76000,30:120000,60:0,90:76000", 120)
        self.assertEqual(custom["rate_schedule"][2], {"start_s": 60, "end_s": 90, "rate_total": 0.0})
        self.assertTrue(custom["rate_total_overridden"])

    def test_invalid_economic_parameters(self):
        for band in ["0", "-1", "1.5", "NaN", "inf", "30000"]:
            with self.assertRaises(ValueError):
                parse_workload(band, ".5", ".05", "", 120)
        for invalid in ["NaN", "inf", "-inf", "-.1", "1.01", "0_1", "０.５", " .5 "]:
            for cross, cancel in [(invalid, ".05"), (".5", invalid)]:
                with self.assertRaises(ValueError):
                    parse_workload("5", cross, cancel, "", 120)

    def test_invalid_schedules(self):
        for raw in ["0", "1:2", "0:NaN", "0:inf", "0:-1", "-1:2", "NaN:2",
                    "0:1,0:2", "0:1,3:2,2:3", "0:1,10:2", "0:1,", "0:1:2", "0:1_000", "0:１０００", "0:1, 2:2"]:
            with self.assertRaises(ValueError, msg=raw):
                parse_workload("5", ".5", ".05", raw, 10)
        with self.assertRaises(ValueError):
            parse_workload("5", ".5", ".05", "0:1", 0)
        maximum = ",".join(f"{s}:1" for s in range(64))
        self.assertEqual(len(parse_workload("5", ".5", ".05", maximum, 100)["rate_schedule"]), 64)
        with self.assertRaises(ValueError):
            parse_workload("5", ".5", ".05", maximum + ",64:1", 100)

    def test_schedule_provenance_requires_complete_matching_timely_records(self):
        import copy
        workload = parse_workload("5", ".5", ".05", "0:10,3:0", 5)
        observed = [dict(phase, index=i, observed_elapsed_s=phase["start_s"] + .01,
                         planned_unix_s=1000 + phase["start_s"])
                    for i, phase in enumerate(workload["rate_schedule"])]
        def check(records, errors=(), manifest=workload, command="bench --rate-schedule 0:10,3:0"):
            return schedule_provenance(manifest, records, errors, command, 5)
        self.assertTrue(check(observed)["valid"])
        self.assertFalse(check(observed[:1])["valid"])
        self.assertFalse(check(list(reversed(observed)))["valid"])
        self.assertFalse(check(observed, ["malformed"])["valid"])
        self.assertFalse(check(observed, manifest=None)["valid"])
        self.assertFalse(check([], manifest=None, command="bench --rate-schedule 0:10")["valid"])
        self.assertFalse(check(observed, command="bench --rate-schedule 0:99")["valid"])
        self.assertFalse(check(observed, command="bench --econ")["valid"])
        self.assertFalse(check(observed, command="bench --rate-schedule 0:10,3:0 --rate-schedule 0:10,3:0")["valid"])
        for key, value in [("rate_total", 99), ("index", True), ("observed_elapsed_s", 3),
                           ("planned_unix_s", 0), ("start_s", float("nan"))]:
            corrupt = copy.deepcopy(observed)
            corrupt[0][key] = value
            self.assertFalse(check(corrupt)["valid"], key)
        self.assertFalse(check([None, observed[1]])["valid"])
        self.assertIsNone(schedule_provenance(None, [], [], "", 5)["valid"])


if __name__ == "__main__":
    unittest.main()
