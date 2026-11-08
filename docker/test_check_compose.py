"""Regression tests for the README/Compose consistency gate."""

import unittest

from check_compose import check


class ComposeExampleTests(unittest.TestCase):
    compose = b"services:\n  apm:\n    image: example:edge\n"
    start = b"<!-- BEGIN COMPOSE -->\n```yaml\n"
    end = b"```\n<!-- END COMPOSE -->"

    def test_matching_example(self):
        check(b"# Install\n" + self.start + self.compose + self.end, self.compose)

    def test_drift_is_rejected(self):
        with self.assertRaisesRegex(ValueError, "differs"):
            check(self.start + self.compose + self.end, self.compose + b"\n")

    def test_missing_marker_is_rejected(self):
        with self.assertRaisesRegex(ValueError, "exactly one"):
            check(self.compose + self.end, self.compose)

    def test_duplicate_block_is_rejected(self):
        block = self.start + self.compose + self.end
        with self.assertRaisesRegex(ValueError, "exactly one"):
            check(block + block, self.compose)

    def test_reversed_markers_are_rejected(self):
        with self.assertRaisesRegex(ValueError, "out of order"):
            check(self.end + self.start, b"")


if __name__ == "__main__":
    unittest.main()
