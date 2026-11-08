#!/usr/bin/env python3
"""Keep the installation example byte-for-byte identical to the Compose file."""

from pathlib import Path
import re
from typing import Any


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


def extract_windows_compose(guide: str) -> str:
    opening = re.search(r"^```yaml[ \t]*$", guide, re.M)
    if not opening:
        raise ValueError("docs/windows.md must contain a fenced YAML Compose example")
    closing = re.search(r"^```[ \t]*$", guide[opening.end() :], re.M)
    if not closing:
        raise ValueError("docs/windows.md YAML Compose fence is not closed")
    end = opening.end() + closing.start()
    return guide[opening.end() : end]


def check_windows_compose(guide: str, config: dict[str, Any]) -> None:
    compose_yaml = extract_windows_compose(guide)
    service = config.get("services", {}).get("apm", {})
    mounts = service.get("volumes", [])
    if "user" in service:
        raise ValueError("Windows Compose example must omit the user key")

    named_data_mount = any(
        mount.get("type") == "volume"
        and mount.get("target") == "/data"
        and isinstance(mount.get("source"), str)
        and bool(mount["source"].strip())
        and isinstance(config.get("volumes"), dict)
        and mount["source"] in config["volumes"]
        and isinstance(config["volumes"][mount["source"]], dict)
        for mount in mounts
    )
    if not named_data_mount:
        raise ValueError(
            "Windows Compose example must mount /data from a declared named volume"
        )

    photo_bind_mount = any(
        mount.get("type") == "bind" and mount.get("target") == "/volumes/photos"
        for mount in mounts
    )
    long_photo_bind = re.search(
        r"(?ms)^ {6}-\s+type:\s*bind\s*\n"
        r"(?:(?!^ {6}-\s).*\n)*?"
        r"^ {8}target:\s*/volumes/photos\s*$",
        compose_yaml,
    )
    if not photo_bind_mount or not long_photo_bind:
        raise ValueError(
            "Windows photo folder must use long bind syntax at /volumes/photos"
        )


if __name__ == "__main__":
    root = Path(__file__).resolve().parent.parent
    check((root / "README.md").read_bytes(), (root / "docker/compose.yml").read_bytes())
    print("README.md Compose example matches docker/compose.yml")
