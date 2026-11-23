#!/usr/bin/env python3
"""Check the documents and the site of this repository. Use only the Python standard library.

Use:
    python3 -I scripts/check-docs.py [CHECK ...]

The checks are: site, links, ste. Without a name, the script runs all of them.
  - site: tools/docs/build_site.py --check. Each page in docs/*.html must agree with its source.
  - links: tools/docs/check_links.py. Each relative link and each anchor must exist.
  - ste: tools/dev/ste-check.py on README.md and on each Markdown file under docs/.
The script makes no network request, and it writes no file.
It shows the output of each check and a summary.
The exit code is 0 when no check finds a problem, 1 when a check fails, and 2 for an unknown check.
"""

from __future__ import annotations

import subprocess
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent


def markdown_documents() -> list[str]:
    """Return README.md and the Markdown files under docs/, relative to the root."""
    files = [ROOT / "README.md", *sorted((ROOT / "docs").rglob("*.md"))]
    return [str(path.relative_to(ROOT)) for path in files if path.is_file()]


def commands() -> dict[str, list[str]]:
    """Return the command line of each check. Each tool runs in isolated mode (-I)."""
    python = [sys.executable, "-I"]
    return {
        "site": [*python, "tools/docs/build_site.py", "--check"],
        "links": [*python, "tools/docs/check_links.py"],
        "ste": [*python, "tools/dev/ste-check.py", *markdown_documents()],
    }


def main(argv: list[str]) -> int:
    """Run the checks that argv names, or all of them. Return the exit code."""
    if any(arg in ("-h", "--help") for arg in argv):
        print(__doc__)
        return 0
    table = commands()
    unknown = [name for name in argv if name not in table]
    if unknown:
        print(f"unknown check: {', '.join(unknown)} (the checks are: {', '.join(table)})", file=sys.stderr)
        return 2
    selected = argv or list(table)
    results: list[tuple[str, int]] = []
    for name in selected:
        command = table[name]
        shown = " ".join(command[2:4]) + (" ..." if len(command) > 4 else "")
        print(f"== {name}: {shown}", flush=True)
        code = subprocess.run(command, cwd=ROOT, check=False).returncode
        results.append((name, code))
        print(f"== {name}: {'ok' if code == 0 else f'FAILED (exit code {code})'}\n", flush=True)
    failed = [name for name, code in results if code != 0]
    print("summary: " + ", ".join(f"{name} {'ok' if code == 0 else 'FAILED'}" for name, code in results))
    return 1 if failed else 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
