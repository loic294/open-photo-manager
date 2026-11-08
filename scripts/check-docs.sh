#!/bin/sh
# Source-only documentation contracts; Python 3 is already required by check-compose.
set -eu
cd "$(dirname "$0")/.."
PYTHONDONTWRITEBYTECODE=1 python3 - <<'PY'
import json
import re
import subprocess
import sys
from pathlib import Path
from urllib.parse import unquote, urlsplit
sys.path.insert(0, str(Path("docker").resolve()))
from check_compose import check_windows_compose, extract_windows_compose

errors = []
def fail(path, line, message):
    errors.append(f"{path}:{line}: {message}")

def prose(text, keep_code=False):
    # Preserve line numbers; fenced examples and inline code are not links or UI labels.
    lines, fence = [], None
    for line in text.splitlines():
        marker = re.match(r"^\s*(`{3,}|~{3,})", line)
        if marker:
            token = marker[1]
            if fence is None:
                fence = token
            elif token[0] == fence[0] and len(token) >= len(fence):
                fence = None
            lines.append("")
        else:
            lines.append("" if fence else line if keep_code else re.sub(r"`+[^`]*`+", "", line))
    return lines

def anchors(path):
    found, counts = set(), {}
    text = path.read_text()
    for line in prose(text, keep_code=True):
        heading = re.match(r"^\s{0,3}#{1,6}\s+(.+?)\s*#*\s*$", line)
        if heading:
            slug = re.sub(r"<[^>]+>", "", heading[1]).lower()
            slug = re.sub(r"[^\w\- ]", "", slug).replace(" ", "-")
            count = counts.get(slug, 0)
            counts[slug] = count + 1
            found.add(slug + (f"-{count}" if count else ""))
    found.update(re.findall(r'\b(?:id|name)=["\']([^"\']+)["\']', text))
    return found

docs = [Path("README.md"), *sorted(Path("docs").glob("*.md"))]
sources = {}
for base, suffix in [("crates", "*.rs"), ("web/src", "*.ts")]:
    for path in Path(base).rglob(suffix):
        if any(p in {"tests", "test", "generated"} for p in path.parts) or ".test." in path.name:
            continue
        if path.name in {"tests.rs", "test_support.rs", "testing.rs"} or path.name.endswith("_tests.rs"):
            continue
        text = path.read_text()
        # Colocated test modules conventionally occupy the remainder of their source file.
        text = re.split(r"#\[cfg\(test\)\]\s*mod\s+\w+\s*\{", text, maxsplit=1)[0]
        sources[path] = text
corpus = re.sub(r"\s+", " ", " ".join(sources.values()))
allow = {}
allow_path = Path("docs/labels-allowlist.txt")
if allow_path.exists():
    for number, line in enumerate(allow_path.read_text().splitlines(), 1):
        if not line.strip() or line.lstrip().startswith("#"):
            continue
        label, sep, comment = line.partition("#")
        if not sep or not comment.strip():
            fail(allow_path, number, "dynamic label needs a # reason")
        allow[label.strip()] = comment.strip()
for path in docs:
    for number, line in enumerate(prose(path.read_text()), 1):
        for target in re.findall(r"!?\[[^\]\n]*\]\(([^)\s]+)(?:\s+[^)]*)?\)", line):
            url = urlsplit(target)
            if url.scheme or url.netloc:
                continue
            dest = path.parent / unquote(url.path) if url.path else path
            if not dest.exists():
                fail(path, number, f"missing relative link: {target}")
            elif url.fragment and (not dest.is_file() or unquote(url.fragment) not in anchors(dest)):
                fail(path, number, f"missing anchor: {target}")
        for label in re.findall(r"\*\*([^*\n]+)\*\*", line):
            plain = re.sub(r"(?:…|\.\.\.|\s*\((?:N|\d+)\))$", "", label).strip()
            if plain not in corpus and label not in allow:
                fail(path, number, f"UI label absent from non-test sources: {label}")
table = Path("docker/README.md")
rows = {name: n for n, line in enumerate(table.read_text().splitlines(), 1)
        for name in re.findall(r"^\|\s*`(APM_[A-Z0-9_]+)`\s*\|", line)}
# No ignored production names: test-only files/modules are excluded above.
used = {}
for path, text in sources.items():
    if path.suffix == ".rs":
        for number, line in enumerate(text.splitlines(), 1):
            for name in re.findall(r"\bAPM_[A-Z0-9_]+\b", line):
                used.setdefault(name, (path, number))
for name in used.keys() - rows.keys():
    fail(*used[name], f"undocumented environment variable: {name}")
for name in rows.keys() - used.keys():
    fail(table, rows[name], f"documented variable absent from production Rust: {name}")
client = Path("crates/apm-server/src/immich/client.rs")
key_match = re.search(r"pub const KEY_PERMISSIONS:.*?= &\[(.*?)\];", client.read_text(), re.S)
if not key_match:
    fail(client, 1, "cannot find API-key permission registry")
requested = re.findall(r'"([^"]+)"', key_match[1]) if key_match else []
registry = Path("crates/apm-server/src/immich/spec-permissions.txt").read_text().splitlines()
guide = Path("docs/immich.md")
text = guide.read_text()
start, end = "<!-- immich-permissions:start -->", "<!-- immich-permissions:end -->"
if text.count(start) != 1 or text.count(end) != 1 or text.index(start) > text.index(end):
    fail(guide, 1, "expected one ordered pair of API-key permission markers")
else:
    block = text.split(start)[1].split(end)[0]
    if re.findall(r"`([^`]+)`", block) != requested or not set(requested) <= set(registry):
        fail(guide, text[:text.index(start)].count("\n") + 1,
             "API-key permissions drifted from client/spec-permissions.txt")

windows_guide = Path("docs/windows.md")
windows_text = windows_guide.read_text()
try:
    compose_yaml = extract_windows_compose(windows_text)
    subprocess.run(
        ["docker", "compose", "-f", "-", "config", "-q"],
        input=compose_yaml,
        text=True,
        check=True,
        capture_output=True,
    )
    resolved = subprocess.run(
        ["docker", "compose", "-f", "-", "config", "--format", "json"],
        input=compose_yaml,
        text=True,
        check=True,
        capture_output=True,
    )
    check_windows_compose(windows_text, json.loads(resolved.stdout))
except (OSError, subprocess.CalledProcessError, json.JSONDecodeError, ValueError) as error:
    detail = getattr(error, "stderr", None) or str(error)
    fail(windows_guide, 1, f"invalid Windows Compose example: {detail.strip()}")
if errors:
    print("\n".join(errors), file=sys.stderr)
    sys.exit(1)
print("Documentation links, UI labels, environment names and Immich permissions match sources")
PY
make check-compose
