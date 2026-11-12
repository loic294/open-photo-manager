"""Regression tests for the README/Compose consistency gate."""

import unittest
from pathlib import Path

from check_compose import check, check_windows_compose, extract_windows_compose


class ComposeExampleTests(unittest.TestCase):
    compose = b"services:\n  opm:\n    image: example:edge\n"
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


class WindowsComposeGuideTests(unittest.TestCase):
    guide = (Path(__file__).resolve().parent.parent / "docs/windows.md").read_text()

    @staticmethod
    def config():
        return {
            "services": {
                "opm": {
                    "volumes": [
                        {
                            "type": "bind",
                            "source": "C:/Users/YOUR_NAME/Pictures",
                            "target": "/volumes/photos",
                        },
                        {
                            "type": "volume",
                            "source": "opm-data",
                            "target": "/data",
                        },
                    ]
                }
            },
            "volumes": {"opm-data": {"name": "opm-data"}},
        }

    def test_accepts_guide_compose_example(self):
        config = self.config()
        check_windows_compose(self.guide, config)
        self.assertIn("services:", extract_windows_compose(self.guide))

    def test_rejects_anonymous_data_volume(self):
        config = self.config()
        del config["services"]["opm"]["volumes"][1]["source"]
        with self.assertRaisesRegex(ValueError, "declared named volume"):
            check_windows_compose(self.guide, config)

    def test_rejects_bind_mounted_data(self):
        config = self.config()
        config["services"]["opm"]["volumes"][1].update(
            type="bind", source="C:/opm-data"
        )
        with self.assertRaisesRegex(ValueError, "declared named volume"):
            check_windows_compose(self.guide, config)

    def test_rejects_user_key(self):
        config = self.config()
        config["services"]["opm"]["user"] = "1000:1000"
        with self.assertRaisesRegex(ValueError, "omit the user key"):
            check_windows_compose(self.guide, config)

    def test_rejects_missing_yaml_fence(self):
        guide = self.guide.replace("```yaml", "```text", 1)
        with self.assertRaisesRegex(ValueError, "fenced YAML"):
            check_windows_compose(guide, self.config())

    def test_rejects_unclosed_yaml_fence(self):
        guide = self.guide.split("```yaml", 1)[0] + "```yaml\nservices:\n"
        with self.assertRaisesRegex(ValueError, "not closed"):
            check_windows_compose(guide, self.config())

    def test_rejects_short_photo_bind_syntax(self):
        guide = self.guide.replace(
            '      - type: bind\n'
            '        source: "C:/Users/YOUR_NAME/Pictures"\n'
            '        target: /volumes/photos\n'
            '        bind:\n'
            '          create_host_path: false',
            '      - "C:/Users/YOUR_NAME/Pictures:/volumes/photos"',
            1,
        )
        self.assertNotEqual(guide, self.guide)
        with self.assertRaisesRegex(ValueError, "long bind syntax"):
            check_windows_compose(guide, self.config())


if __name__ == "__main__":
    unittest.main()
