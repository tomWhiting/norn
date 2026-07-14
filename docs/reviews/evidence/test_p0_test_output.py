"""Regression tests for retained Rust failure identities."""

from __future__ import annotations

import unittest

from p0_test_output import (
    exact_unittest_count,
    failure_identity_report,
    stable_test_identity,
    test_summary_lines,
)


def failed_block(
    *names: str,
    passed: int = 0,
    failed: int | None = None,
    package: str = "norn",
    selector: str = "--lib",
    target: str | None = None,
) -> str:
    declared = len(names) if failed is None else failed
    indented = "\n".join(f"    {name}" for name in names)
    command = f"-p {package} {selector}" + (f" {target}" if target else "")
    return (
        f"failures:\n{indented}\n\n"
        f"test result: FAILED. {passed} passed; {declared} failed; 0 ignored;\n"
        f"error: test failed, to rerun pass '{command}'\n"
    )


class FailedTestNamesTests(unittest.TestCase):
    def test_unittest_count_requires_one_unsuffixed_success_marker(self) -> None:
        self.assertEqual(exact_unittest_count("", "Ran 30 tests in 0.01s\nOK\n"), 30)
        self.assertIsNone(exact_unittest_count("", "Ran 0 tests in 0.00s\n"))
        self.assertIsNone(
            exact_unittest_count("", "Ran 30 tests in 0.01s\nOK (skipped=1)\n")
        )
        self.assertIsNone(
            exact_unittest_count(
                "Ran 30 tests in 0.01s\nOK\n",
                "Ran 30 tests in 0.01s\nOK\n",
            )
        )

    def test_final_failure_list_is_scoped_to_its_summary(self) -> None:
        output = """
failures:

---- crate::tests::first stdout ----
private diagnostic body

failures:
    crate::tests::first
    crate::tests::second

test result: FAILED. 10 passed; 2 failed; 0 ignored;
error: test failed, to rerun pass '-p norn --lib'
"""

        report = failure_identity_report(output, "")

        self.assertEqual(report.names, ("crate::tests::first", "crate::tests::second"))
        self.assertTrue(report.complete)

    def test_indented_diagnostic_secret_is_not_an_identity(self) -> None:
        output = """
failures:

---- crate::tests::fails stdout ----
    PRIVATE_TOKEN_DO_NOT_RETAIN
"""

        report = failure_identity_report(output, "")

        self.assertEqual(report.names, ())
        self.assertFalse(report.complete)

    def test_truncated_output_uses_strict_status_fallback(self) -> None:
        output = """
test crate::tests::passes ... ok
test crate::tests::fails ... FAILED
"""

        report = failure_identity_report("", output)

        self.assertEqual(report.names, ("crate::tests::fails",))
        self.assertEqual(report.groups[0].source, "status_fallback")
        self.assertFalse(report.complete)

    def test_count_mismatch_is_retained_but_incomplete(self) -> None:
        report = failure_identity_report(
            failed_block("crate::tests::first", failed=2), ""
        )

        self.assertEqual(report.names, ("crate::tests::first",))
        self.assertFalse(report.complete)

    def test_multiple_binaries_keep_separate_complete_groups(self) -> None:
        output = failed_block("alpha::tests::fails") + failed_block(
            "beta::tests::fails",
            passed=3,
            package="beta",
            selector="--test",
            target="integration",
        )

        report = failure_identity_report(output, "")

        self.assertEqual(len(report.groups), 2)
        self.assertEqual(report.names, ("alpha::tests::fails", "beta::tests::fails"))
        self.assertTrue(report.complete)

    def test_cross_stream_duplicate_names_are_valid_separate_binaries(self) -> None:
        first = failed_block("shared::tests::same_name", package="alpha")
        second = failed_block(
            "shared::tests::same_name",
            package="beta",
            selector="--test",
            target="integration",
        )

        report = failure_identity_report(first, second)

        self.assertEqual(
            report.names,
            ("shared::tests::same_name", "shared::tests::same_name"),
        )
        self.assertEqual(len(report.groups), 2)
        self.assertTrue(report.complete)

    def test_duplicate_within_one_binary_is_incomplete(self) -> None:
        report = failure_identity_report(
            failed_block("crate::tests::same", "crate::tests::same"), ""
        )

        self.assertFalse(report.complete)

    def test_unmatched_status_after_complete_summary_marks_truncation(self) -> None:
        output = (
            "test first::tests::fails ... FAILED\n"
            + failed_block("first::tests::fails")
            + "test second::tests::fails ... FAILED\n"
        )

        report = failure_identity_report(output, "")

        self.assertEqual(report.names, ("first::tests::fails", "second::tests::fails"))
        self.assertFalse(report.complete)

    def test_only_stable_non_secret_identity_shapes_are_accepted(self) -> None:
        self.assertTrue(stable_test_identity("r#loop::tests::failure"))
        self.assertTrue(
            stable_test_identity(
                "crates/norn/src/lib.rs - module::Type::method (line 42) - compile fail"
            )
        )
        self.assertFalse(stable_test_identity("API_KEY=private-value"))
        self.assertFalse(
            stable_test_identity("/Users/operator/private.rs - test (line 1)")
        )

    def test_rerun_hint_binds_package_and_target_without_retaining_path(self) -> None:
        report = failure_identity_report(
            failed_block(
                "crate::tests::fails",
                package="norn-cli",
                selector="--test",
                target="index_lock_deadline",
            ),
            "",
        )

        self.assertTrue(report.complete)
        self.assertEqual(
            report.groups[0].as_record()["target"],
            {
                "package": "norn-cli",
                "kind": "test",
                "name": "index_lock_deadline",
            },
        )

    def test_backtick_rerun_hint_on_stderr_binds_stdout_summary(self) -> None:
        stdout = failed_block("crate::tests::fails").split("error:", 1)[0]
        stderr = "error: test failed, to rerun pass `-p norn --lib`\n"

        report = failure_identity_report(stdout, stderr)

        self.assertTrue(report.complete)
        self.assertEqual(
            report.groups[0].as_record()["target"],
            {"package": "norn", "kind": "lib", "name": None},
        )

    def test_doctest_rerun_hint_binds_the_doc_target(self) -> None:
        output = failed_block("crate::docs::fails").replace(
            "error: test failed, to rerun pass '-p norn --lib'",
            "error: doctest failed, to rerun pass '-p norn --doc'",
        )

        report = failure_identity_report(output, "")

        self.assertTrue(report.complete)
        self.assertEqual(
            report.groups[0].as_record()["target"],
            {"package": "norn", "kind": "doc", "name": None},
        )

    def test_missing_or_malformed_rerun_hint_is_incomplete(self) -> None:
        missing = failed_block("crate::tests::fails").split("error:", 1)[0]
        hostile = failed_block("crate::tests::fails").replace(
            "-p norn --lib", "-p norn --test /Users/operator/private"
        )

        self.assertFalse(failure_identity_report(missing, "").complete)
        self.assertFalse(failure_identity_report(hostile, "").complete)

    def test_duplicate_target_across_groups_is_incomplete(self) -> None:
        output = failed_block("alpha::tests::fails") + failed_block(
            "beta::tests::fails"
        )

        self.assertFalse(failure_identity_report(output, "").complete)

    def test_only_stable_summary_shape_is_retained(self) -> None:
        stable = (
            "test result: FAILED. 3 passed; 1 failed; 0 ignored; "
            "0 measured; 2 filtered out; finished in 0.01s"
        )
        hostile = stable + " /Users/operator/private"

        self.assertEqual(test_summary_lines(f"{hostile}\n{stable}\n", ""), [stable])


if __name__ == "__main__":
    unittest.main()
