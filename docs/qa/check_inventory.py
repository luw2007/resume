#!/usr/bin/env python3
"""Integrity check for docs/qa/feature-inventory.csv.

The inventory's line-number citations rotted silently across 60 commits before
this check existed, so drift is verified mechanically rather than by review:
every `path:line (symbol)` anchor must still declare that symbol on that line.

Usage:  python3 docs/qa/check_inventory.py [repo-root]
Exit 0 when the inventory is internally consistent, 1 otherwise.
"""
import csv
import pathlib
import re
import sys
from collections import Counter

REQUIRED_COLUMNS = [
    "feature_id", "area", "feature_name", "user_story", "expected_behaviour",
    "how_to_test", "spec_ref", "code_ref", "status", "error_notes",
    "fix_ref", "retest_status",
]
VALID_STATUS = {"Untested", "Pass", "Fail", "Blocked"}
VALID_RETEST = {"", "N/A", "Pass", "Fail", "Blocked"}

ANCHOR = re.compile(r'^(?P<path>[A-Za-z0-9_./-]+\.rs):(?P<line>\d+) \((?P<symbol>[^()]+)\)$')
DECL = re.compile(
    r'^\s*(?:pub(?:\([^)]*\))?\s+)?(?:async\s+|unsafe\s+|const\s+|extern\s+"[^"]*"\s+)*'
    r'(?:fn|struct|enum|trait|type|mod|const|static)\s+(?P<name>[A-Za-z_][A-Za-z0-9_]*)'
)


def check_anchor(root, ref):
    """Return an error string, or None when the anchor resolves."""
    m = ANCHOR.match(ref)
    if not m:
        return f"malformed anchor {ref!r} (want 'path.rs:LINE (symbol)')"
    path, line, symbol = m["path"], int(m["line"]), m["symbol"]
    f = root / path
    if not f.is_file():
        return f"{path} does not exist"
    lines = f.read_text(encoding="utf-8", errors="replace").splitlines()
    if not 1 <= line <= len(lines):
        return f"{path}:{line} is past end of file ({len(lines)} lines)"
    decl = DECL.match(lines[line - 1])
    if not decl:
        return f"{path}:{line} is not a declaration: {lines[line - 1].strip()[:60]!r}"
    if decl["name"] != symbol:
        return f"{path}:{line} declares {decl['name']!r}, anchor claims {symbol!r}"
    return None


def main():
    root = pathlib.Path(sys.argv[1] if len(sys.argv) > 1 else ".").resolve()
    path = root / "docs/qa/feature-inventory.csv"
    with path.open(newline="") as fh:
        reader = csv.DictReader(fh)
        if reader.fieldnames != REQUIRED_COLUMNS:
            print(f"FAIL: columns are {reader.fieldnames}, expected {REQUIRED_COLUMNS}")
            return 1
        rows = list(reader)

    errors = []
    seen = set()
    for r in rows:
        fid = r["feature_id"]
        if fid in seen:
            errors.append(f"{fid}: duplicate feature_id")
        seen.add(fid)
        for col in ("feature_name", "user_story", "expected_behaviour", "how_to_test", "code_ref"):
            if not r[col].strip():
                errors.append(f"{fid}: empty {col}")
        if r["status"] not in VALID_STATUS:
            errors.append(f"{fid}: status {r['status']!r} not in {sorted(VALID_STATUS)}")
        if r["retest_status"] not in VALID_RETEST:
            errors.append(f"{fid}: retest_status {r['retest_status']!r} not in {sorted(VALID_RETEST)}")
        if r["status"] == "Fail" and not r["error_notes"].strip():
            errors.append(f"{fid}: status Fail requires error_notes")
        for ref in (x.strip() for x in r["code_ref"].split(";")):
            if not ref:
                continue
            problem = check_anchor(root, ref)
            if problem:
                errors.append(f"{fid}: {problem}")

    print(f"{len(rows)} rows, {len(seen)} unique ids")
    print("area:   " + ", ".join(f"{k}={v}" for k, v in sorted(Counter(r["area"] for r in rows).items())))
    print("status: " + ", ".join(f"{k}={v}" for k, v in sorted(Counter(r["status"] for r in rows).items())))
    retest = Counter(r["retest_status"] or "(blank)" for r in rows)
    print("retest: " + ", ".join(f"{k}={v}" for k, v in sorted(retest.items())))
    if errors:
        print(f"\nFAIL: {len(errors)} problem(s)")
        for e in errors:
            print("  " + e)
        return 1
    print("\nOK")
    return 0


if __name__ == "__main__":
    sys.exit(main())
