"""Unit tests for the parts of the harness that need no binary: run them with
`python3 -m unittest discover -s bench/bakeoff/tests -t bench/bakeoff`."""

import json
import unittest

from bakeoff.classify import classify, duplicate_side_effects, redundant_writes
from bakeoff.controls import CONTROLS, Script, control_script
from bakeoff.proxy import parse_sse, sse_body, summarize_decision
from bakeoff.report import paired, wilson
from bakeoff.suite import load_suite
from bakeoff.util import redact_value


def run_record(**over):
    base = {
        "report": {"verified": True, "terminal_state": "completed", "outcome_reason": None,
                   "executions_by_capability": {"project.write": 1, "pax.test": 1}, "audit": {"clean": True},
                   "malformed_replies": 0},
        "verification": {"changed_files": {"crates/ledger-core/src/amount.rs": "M"}, "out_of_scope": [],
                         "protected_changed": [], "acceptance": {"passed": True}, "regressions": [], "visible_unfixed": []},
        "processes": [{"exit": 0, "killed": None}], "calls": [[]], "intents": None, "settlements": [],
    }
    for k, v in over.items():
        if isinstance(v, dict) and isinstance(base.get(k), dict):
            base[k] = {**base[k], **v}
        else:
            base[k] = v
    return base


class Harness(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.suite = load_suite()
        cls.task = cls.suite.task("ledger-trace-decimal")

    def test_the_suite_has_twenty_development_and_held_out_tasks(self):
        dev = [t for t in self.suite.tasks if t["split"] == "dev"]
        held = [t for t in self.suite.tasks if t["split"] == "heldout"]
        self.assertGreaterEqual(len(self.suite.tasks), 20)
        self.assertTrue(dev and held)
        for t in self.suite.tasks:
            self.assertLessEqual(len(t["goal"].encode()), 1000, t.id)

    def test_streamed_replies_round_trip(self):
        body = sse_body(1, '{"decision":"complete","summary":"x"}', {"prompt_tokens": 3, "completion_tokens": 2})
        content, usage, finish = parse_sse(body)
        self.assertEqual(json.loads(content)["decision"], "complete")
        self.assertEqual(usage["prompt_tokens"], 3)
        self.assertEqual(finish, "stop")

    def test_decision_summaries_keep_no_content(self):
        d = summarize_decision(json.dumps({"decision": "request_capability", "capability": "project.write",
                                           "inputs": {"path": "src/a.rs", "content": "SECRET CODE"}}))
        self.assertEqual(d["path"], "src/a.rs")
        self.assertNotIn("SECRET", json.dumps(d))
        self.assertTrue(d["content_digest"])
        self.assertIsNone(summarize_decision(json.dumps({"inputs": {"path": "/etc/passwd"}}))["path"])
        self.assertFalse(summarize_decision("Sure, I fixed it.")["parsed"])

    def test_a_verified_accepted_scoped_run_is_a_success(self):
        o = classify(self.task, run_record())
        self.assertTrue(o["success"])
        self.assertEqual(o["failure_class"], "none")

    def test_model_claims_never_count(self):
        r = run_record(report={"verified": False, "terminal_state": "blocked"}, processes=[{"exit": 1, "killed": None}],
                       calls=[[{"decision": {"decision": "complete"}}]])
        o = classify(self.task, r)
        self.assertFalse(o["success"])
        self.assertTrue(o["claimed_but_not_verified"])

    def test_host_verified_but_rejected_is_a_false_completion(self):
        o = classify(self.task, run_record(verification={"acceptance": {"passed": False}}))
        self.assertEqual(o["failure_class"], "false_completion")

    def test_safety_beats_everything(self):
        o = classify(self.task, run_record(verification={"protected_changed": ["CHANGELOG.md"]}))
        self.assertFalse(o["success"])
        self.assertEqual(o["failure_class"], "safety_violation")

    def test_budget_comes_from_the_runtime_not_the_models_words(self):
        words = classify(self.task, run_record(report={"verified": False, "terminal_state": "escalated",
                                                       "outcome_reason": "stores only up to `limit`"},
                                               processes=[{"exit": 1, "killed": None}]))
        self.assertFalse(words["budget_exhausted"])
        limit = classify(self.task, run_record(report={"verified": False, "terminal_state": "limit_reached",
                                                       "outcome_reason": "turns limit reached"},
                                               processes=[{"exit": 1, "killed": None}]))
        self.assertEqual(limit["failure_class"], "budget_exhausted")

    def test_rate_limits_are_infrastructure(self):
        o = classify(self.task, run_record(report={"verified": False, "terminal_state": "failed"},
                                           processes=[{"exit": 4, "killed": None}],
                                           calls=[[{"upstream_status": 429, "decision": None}]]))
        self.assertTrue(o["infra"])

    def test_duplicates_span_attempts_and_redundant_writes_do_not(self):
        same = "project.write key=k binding=b1.x"
        within = {"intents": [{"key": "a2.t-exec-1", "action": same, "result": "succeeded"},
                              {"key": "a2.t-exec-3", "action": same, "result": "succeeded"}]}
        across = {"intents": [{"key": "a2.t-exec-1", "action": same, "result": "succeeded"},
                              {"key": "a3.t-exec-1", "action": same, "result": "succeeded"}]}
        self.assertEqual(duplicate_side_effects(within), 0)
        self.assertEqual(duplicate_side_effects(across), 1)
        w = {"decision": {"capability": "project.write", "path": "a", "content_digest": "d"}}
        self.assertEqual(redundant_writes({"calls": [[w, w]]}), 1)
        undelivered = {**w, "delivered": False}
        self.assertEqual(duplicate_side_effects({"calls": [[undelivered], [w]]}), 0)
        self.assertEqual(duplicate_side_effects({"calls": [[w], [w]]}), 1)

    def test_a_script_resumes_where_it_stopped(self):
        s = Script(control_script(self.task, "reference"))
        first = s({}, 1)
        s.delivered()
        self.assertNotEqual(s({}, 1), first)
        for c in CONTROLS:
            if c not in ("vandal", "gamer"):
                self.assertTrue(control_script(self.task, c))

    def test_redaction(self):
        import os
        os.environ["BAKEOFF_UPSTREAM_KEY"] = "sk-test-0123456789abcdef"
        v = redact_value({"x": "key sk-test-0123456789abcdef and Bearer abcdefghijklmnop"})
        self.assertNotIn("sk-test", json.dumps(v))
        self.assertNotIn("abcdefghijklmnop", json.dumps(v))

    def test_intervals_and_pairing(self):
        self.assertIsNone(wilson(0, 0))
        lo, hi = wilson(5, 10)
        self.assertLess(lo, 0.5)
        self.assertGreater(hi, 0.5)
        runs = []
        for t, a, b in (("t1", 0, 1), ("t2", 0, 1), ("t3", 1, 1)):
            for cfg, ok in (("base", a), ("new", b)):
                runs.append({"model": {"id": "m"}, "config": cfg, "task_id": t, "outcome": {"success": bool(ok)}})
        p = paired(runs, "base", "new", "m")
        self.assertEqual((p["tasks_better"], p["tasks_same"], p["tasks_worse"]), (2, 1, 0))


if __name__ == "__main__":
    unittest.main()
