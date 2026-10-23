"""Known-answer tests for analyze_v5. Run: python3 -I test_analyze_v5.py"""
import os
import sys
import unittest

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import analyze_v5 as a  # noqa: E402

TEST = {"file": "tests/t.rs", "function": "the_test", "via": "start"}
F = {"file": "src/a.rs", "function": "Service::handle", "via": "method"}
G = {"file": "src/b.rs", "function": "Gate::check", "via": "trait_object"}
T = {"file": "src/c.rs", "function": "target", "via": "call"}
PATH = {"target": 0, "steps": [TEST, F, G, T]}


def out(cands, *, conceal=True, stop="complete", concealment=(), hidden=(), excluded=()):
    return {
        "ranking": {
            "top": [{"id": c} for c in cands],
            "frontier": {
                "could_conceal_target": conceal,
                "stop": stop,
                "concealment": list(concealment),
                "hidden_examples": list(hidden),
                "excluded": list(excluded),
            },
        }
    }


class Material(unittest.TestCase):
    def test_gap_and_predecessor(self):
        ok, _, gap, pred = a.material_report(PATH, out(["symbol:src/a.rs:Service::handle"], conceal=False))
        self.assertFalse(ok)
        self.assertEqual((gap["function"], pred["function"]), ("Gate::check", "Service::handle"))

    def test_flag_without_a_matching_report_is_not_enough(self):
        o = out(["symbol:src/a.rs:Service::handle"], excluded=[{"from": "other", "plausible": ["symbol:src/z.rs:z"]}])
        self.assertFalse(a.material_report(PATH, o)[0])

    def test_each_accepted_report(self):
        base = ["symbol:src/a.rs:Service::handle"]
        self.assertTrue(a.material_report(PATH, out(base, stop="candidate_budget"))[0])
        self.assertTrue(a.material_report(PATH, out(base, concealment=["tests/t.rs does not compile, so none of its tests ran"]))[0])
        self.assertTrue(a.material_report(PATH, out(base, hidden=["symbol:src/b.rs:Gate::check"]))[0])
        self.assertTrue(a.material_report(PATH, out(base, excluded=[{"from": "x", "plausible": ["symbol:src/c.rs:target"]}]))[0])
        self.assertTrue(a.material_report(PATH, out(base, excluded=[{"from": "Service::handle", "plausible": []}]))[0])
        self.assertTrue(a.material_report(PATH, out(base, concealment=["1 calls ...; e.g. Service::handle → .check: ..."]))[0])

    def test_reached_path(self):
        o = out(["symbol:src/a.rs:Service::handle", "symbol:src/b.rs:Gate::check", "symbol:src/c.rs:target"], conceal=False)
        self.assertTrue(a.material_report(PATH, o)[0])

    def test_test_helpers_are_not_gaps(self):
        helper = {"file": "tests/t.rs", "function": "fixture", "via": "call"}
        p = {"target": 0, "steps": [TEST, helper, F, T]}
        _, _, gap, pred = a.material_report(p, out([], conceal=False))
        self.assertEqual((gap["function"], pred["function"]), ("Service::handle", "fixture"))


class Metrics(unittest.TestCase):
    def test_ranking_metrics(self):
        m = a.ranking_metrics([1, 4, None, 12])
        self.assertEqual((m["top1"], m["top5"], m["top10"]), (0.25, 0.5, 0.5))
        self.assertEqual(m["mrr"], round((1 + 0.25 + 1 / 12) / 4, 3))
        self.assertEqual(m["mean_rank_found"], round(17 / 3, 1))

    def test_decision(self):
        crit = {
            "P1_no_silent_miss": True,
            "P2_no_false_uniqueness": True,
            "P3_no_false_impossibility": True,
            "P4_recall_deep_dispatch": 1.0,
            "P4_pass": True,
            "P5_bounded_cost": True,
            "fixtures_errored": [],
        }
        good = {"category": "deep", "tags": [], "ranks": [1], "material_omissions": [], "p6_violation": False, "p2_ambiguous_violation": False}
        self.assertEqual(a.decide(crit, [good])["decision"], "PASS")
        for bad in [
            dict(good, p6_violation=True),
            dict(good, p2_ambiguous_violation=True),
            dict(good, material_omissions=[{"reported": False}]),
        ]:
            self.assertEqual(a.decide(crit, [bad])["decision"], "FAIL")
        self.assertEqual(a.decide(dict(crit, P4_pass=False), [good])["decision"], "FAIL")
        self.assertEqual(a.decide(dict(crit, P3_no_false_impossibility=False), [good])["decision"], "FAIL")


if __name__ == "__main__":
    unittest.main()
