#!/usr/bin/env python3
"""Keep the installation example byte-for-byte identical to the Compose file."""

from pathlib import Path


def check(readme: bytes, compose: bytes) -> None:
    start = b"<!-- BEGIN COMPOSE -->\n```yaml\n"
    end = b"```\n<!-- END COMPOSE -->"
    if readme.count(start) != 1 or readme.count(end) != 1:
        raise ValueError("README.md must contain exactly one marked Compose block")
    remainder = readme.split(start, 1)[1]
    if end not in remainder:
        raise ValueError("README.md Compose markers are out of order")
    block = remainder.split(end, 1)[0]
    if block != compose:
        raise ValueError("README.md Compose example differs from docker/compose.yml")


if __name__ == "__main__":
    root = Path(__file__).resolve().parent.parent
    check((root / "README.md").read_bytes(), (root / "docker/compose.yml").read_bytes())
    print("README.md Compose example matches docker/compose.yml")
