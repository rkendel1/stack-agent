"""Regression tests for score_v4. Run: python3 -I -m unittest test_score_v4 (from this directory)
or python3 -I test_score_v4.py."""
import os
import sys
import unittest

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import score_v4 as s  # noqa: E402


def t(file, function):
    return {"file": file, "function": function}


class ParseIds(unittest.TestCase):
    def test_methods_keep_their_type(self):
        self.assertEqual(
            s.parse_symbol_id("symbol:exprlang/src/lexer.rs:Lexer::read_ident"),
            ("exprlang/src/lexer.rs", "Lexer::read_ident", None),
        )

    def test_nested_module_and_type_paths(self):
        self.assertEqual(
            s.parse_symbol_id("symbol:src/a/b.rs:outer::inner::Type::method"),
            ("src/a/b.rs", "outer::inner::Type::method", None),
        )

    def test_ordinary_functions(self):
        self.assertEqual(s.parse_symbol_id("symbol:src/lib.rs:parse_line"), ("src/lib.rs", "parse_line", None))

    def test_duplicate_suffix(self):
        self.assertEqual(
            s.parse_symbol_id("symbol:src/credentials.rs:write_private#2"),
            ("src/credentials.rs", "write_private", 2),
        )
        self.assertEqual(
            s.parse_symbol_id("symbol:src/x.rs:Type::m#10"), ("src/x.rs", "Type::m", 10)
        )

    def test_other_delimiters_in_paths_and_names(self):
        # Dots, dashes and nested directories in the path; generics-like impl type names.
        self.assertEqual(
            s.parse_symbol_id("symbol:crates/compute-provider/src/v1.2/lib.rs:RemoteService::respond"),
            ("crates/compute-provider/src/v1.2/lib.rs", "RemoteService::respond", None),
        )
        self.assertEqual(
            s.parse_symbol_id("symbol:src/lib.rs:Box<dynGate>::admit"),
            ("src/lib.rs", "Box<dynGate>::admit", None),
        )
        # A directory whose name ends in `.rs` does not end the path early.
        self.assertEqual(
            s.parse_symbol_id("symbol:tools.rs/src/main.rs:run"), ("tools.rs/src/main.rs", "run", None)
        )

    def test_malformed_ids_are_rejected(self):
        for bad in [
            "src/lib.rs:f",
            "symbol:src/lib.rs",
            "symbol:src/lib.rs:",
            "symbol:src/lib.rs:Type:::m",
            "symbol:src/lib.rs:Type::",
            "symbol:src/lib.py:f",
        ]:
            with self.assertRaises(s.BadId, msg=bad):
                s.parse_symbol_id(bad)

    def test_the_v3_defect_is_gone(self):
        # v3 split `symbol:<path>:Type::method` with rsplit(":", 1), giving path `...:Type:`.
        path, qualified, _ = s.parse_symbol_id("symbol:src/version.rs:Version::cmp")
        self.assertEqual((path, qualified), ("src/version.rs", "Version::cmp"))


class MatchTargets(unittest.TestCase):
    def test_method_and_function_targets(self):
        self.assertTrue(s.matches("symbol:src/version.rs:Version::cmp", t("src/version.rs", "Version::cmp")))
        self.assertTrue(s.matches("symbol:src/pricing.rs:tier_discount_pct", t("src/pricing.rs", "tier_discount_pct")))

    def test_whole_names_only(self):
        # Never by last segment, and never across types or files.
        self.assertFalse(s.matches("symbol:src/version.rs:Version::cmp", t("src/version.rs", "cmp")))
        self.assertFalse(s.matches("symbol:src/version.rs:cmp", t("src/version.rs", "Version::cmp")))
        self.assertFalse(s.matches("symbol:src/version.rs:Other::cmp", t("src/version.rs", "Version::cmp")))
        self.assertFalse(s.matches("symbol:src/other.rs:Version::cmp", t("src/version.rs", "Version::cmp")))
        self.assertFalse(s.matches("symbol:src/my_version.rs:Version::cmp", t("version.rs", "Version::cmp")))

    def test_equivalent_spellings_match(self):
        self.assertTrue(s.matches("symbol:./src/a.rs:Type::m", t("src/a.rs", "Type :: m")))
        self.assertTrue(s.matches("symbol:fixture/src/a.rs:m#2", t("src/a.rs", "m")))
        self.assertTrue(s.matches("symbol:exprlang/src/eval.rs:compare_values", t("exprlang/src/eval.rs", "compare_values")))


def out(top, decision, *, conceal=False, stop="complete", rank_ms=1):
    return {
        "ranking": {
            "top": [{"id": i} for i in top],
            "total_candidates": len(top),
            "shape": {"expressible": "possible", "reasons": []},
            "frontier": {
                "could_conceal_target": conceal,
                "concealment": [],
                "stop": stop,
                "explored": 1,
                "edges": 1,
                "max_depth_reached": 1,
                "cut_off": 0,
                "excluded_counts": {},
                "unresolved_dispatch": False,
            },
        },
        "decision": decision,
        "cost": {"rank_ms": rank_ms, "index_ms": 1},
        "acceptance": {"timed_out": False, "diagnostics": []},
    }


def gt(category, fix_kind, *targets):
    return {"category": category, "fix_kind": fix_kind, "true_targets": list(targets)}


class Criteria(unittest.TestCase):
    A = "symbol:src/a.rs:Type::target"
    B = "symbol:src/a.rs:other"
    T = t("src/a.rs", "Type::target")

    def test_known_good_set_passes(self):
        r = [
            s.score("f1", gt("deep", "single_body", self.T), out([self.B, self.A], {"kind": "ambiguous", "symbols": [self.B, self.A]})),
            s.score("f2", gt("dispatch", "single_body", self.T), out([self.A], {"kind": "single", "symbols": self.A})),
        ]
        c = s.criteria(r)
        self.assertEqual([x["targets"][0]["rank"] for x in r], [2, 1])
        self.assertTrue(c["all_pass"], c)

    def test_silent_miss_fails_p1_and_flagged_miss_does_not(self):
        silent = s.score("f", gt("deep", "single_body", self.T), out([self.B], {"kind": "ambiguous", "symbols": [self.B]}))
        flagged = s.score("f", gt("deep", "single_body", self.T), out([self.B], {"kind": "ambiguous", "symbols": [self.B]}, conceal=True))
        self.assertFalse(s.criteria([silent])["P1_no_silent_miss"])
        self.assertTrue(s.criteria([flagged])["P1_no_silent_miss"])

    def test_wrong_single_and_single_on_multi_fail_p2(self):
        wrong = s.score("f", gt("deep", "single_body", self.T), out([self.B, self.A], {"kind": "single", "symbols": self.B}))
        multi = s.score("f", gt("multi", "multi_body", self.T), out([self.A], {"kind": "single", "symbols": self.A}))
        self.assertFalse(s.criteria([wrong])["P2_no_false_uniqueness"])
        self.assertFalse(s.criteria([multi])["P2_no_false_uniqueness"])

    def test_false_impossibility_fails_p3(self):
        r = s.score("f", gt("dispatch", "single_body", self.T), out([self.A], {"kind": "not_expressible", "symbols": ["x"]}))
        self.assertFalse(s.criteria([r])["P3_no_false_impossibility"])

    def test_recall_threshold_and_cost(self):
        hit = s.score("f", gt("deep", "single_body", self.T), out([self.A, self.B], {"kind": "ambiguous", "symbols": []}))
        miss = s.score("g", gt("deep", "single_body", self.T), out([self.B], {"kind": "ambiguous", "symbols": []}, conceal=True))
        self.assertEqual(s.criteria([hit, miss] + [hit] * 3)["P4_recall_deep_dispatch"], 0.8)
        self.assertTrue(s.criteria([hit, miss] + [hit] * 3)["P4_pass"])
        self.assertFalse(s.criteria([hit, miss])["P4_pass"])
        slow = s.score("f", gt("deep", "single_body", self.T), out([self.A], {"kind": "ambiguous", "symbols": []}, rank_ms=10_001))
        unreported = s.score("f", gt("deep", "single_body", self.T), out([self.A], {"kind": "ambiguous", "symbols": []}, stop="visit_budget"))
        self.assertFalse(s.criteria([slow])["P5_bounded_cost"])
        self.assertFalse(s.criteria([unreported])["P5_bounded_cost"])


class Workspace(unittest.TestCase):
    def test_a_directory_below_a_manifest_is_detected(self):
        import tempfile

        with tempfile.TemporaryDirectory() as root:
            open(os.path.join(root, "Cargo.toml"), "w").write("[workspace]\n")
            inner = os.path.join(root, "docs", "run", "f01")
            os.makedirs(inner)
            self.assertEqual(s.enclosing_manifest(inner), os.path.join(root, "Cargo.toml"))
            with self.assertRaises(SystemExit):
                s.run("/bin/false", root, os.path.join(root, "out"), work_root=os.path.join(root, "docs"))

    def test_a_clean_temporary_directory_is_not_inside_one(self):
        import tempfile

        with tempfile.TemporaryDirectory() as root:
            self.assertIsNone(s.enclosing_manifest(os.path.join(root, "f01")))


if __name__ == "__main__":
    unittest.main()
