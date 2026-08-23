#!/usr/bin/env python3
"""Integrity check for docs/qa/feature-inventory.csv.

The inventory's line-number citations rotted silently across 60 commits before
this check existed, so drift is verified mechanically rather than by review:
every `path:line (symbol)` anchor must still declare that symbol on that line,
and every `§N Heading` spec reference must name a heading that really lives
under that numbered section of docs/product-design.md.

Usage:  python3 docs/qa/check_inventory.py [repo-root] [--fix]
Exit 0 when the inventory is internally consistent, 1 otherwise.

`--fix` re-resolves every anchor by looking its symbol up in the file and
rewriting the line number. Editing a source file shifts the anchors below the
edit, and repointing several hundred of them by hand is how they rotted in the
first place; the symbol is the durable half of the citation, the number is not.
An anchor whose symbol no longer exists is left alone and reported -- that one
is a real content change, not drift.
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
SPEC_REF = re.compile(r'^§(?P<number>\d+) (?P<heading>.+)$')
HEADING = re.compile(r'^(?P<hashes>#{2,3}) (?:(?P<number>\d+)\. )?(?P<title>.+?)\s*$')
# A line number in prose carries no symbol, so nothing can verify it and
# `--fix` cannot repair it. `code_ref` is the one column allowed to hold them.
PROSE_LINE_REF = re.compile(r'[A-Za-z0-9_./-]+\.rs:\d+|(?<![A-Za-z0-9_]):\d+(?:-\d+)?'
                            r'|\b(?:at|around|near)(?:\s+lines?)?\s+\d{2,4}(?:-\d+)?\b')
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


def reanchor(root, ref):
    """Return the anchor with its line number re-resolved from the symbol.

    Returns (new_ref, None) when the symbol was found, or (ref, reason) when it
    was not and the anchor has to be looked at by a human. A symbol declared
    more than once in one file resolves to the declaration nearest the recorded
    line, which is what keeps a small edit from silently retargeting an anchor
    at a same-named item elsewhere in the file.
    """
    m = ANCHOR.match(ref)
    if not m:
        return ref, f"malformed anchor {ref!r}"
    path, line, symbol = m["path"], int(m["line"]), m["symbol"]
    f = root / path
    if not f.is_file():
        return ref, f"{path} does not exist"
    lines = f.read_text(encoding="utf-8", errors="replace").splitlines()
    found = [n for n, text in enumerate(lines, 1)
             if (d := DECL.match(text)) and d["name"] == symbol]
    if not found:
        return ref, f"{path} no longer declares {symbol!r}"
    best = min(found, key=lambda n: abs(n - line))
    return f"{path}:{best} ({symbol})", None


def fix_anchors(root, path):
    """Rewrite every stale anchor in the inventory. Returns (fixed, unfixable)."""
    with path.open(newline="") as fh:
        reader = csv.DictReader(fh)
        fields = reader.fieldnames
        rows = list(reader)
    fixed, unfixable = 0, []
    for r in rows:
        refs = [x.strip() for x in r["code_ref"].split(";") if x.strip()]
        rewritten = []
        for ref in refs:
            if check_anchor(root, ref) is None:
                rewritten.append(ref)
                continue
            new_ref, reason = reanchor(root, ref)
            if reason:
                unfixable.append(f"{r['feature_id']}: {reason}")
            else:
                fixed += 1
            rewritten.append(new_ref)
        r["code_ref"] = "; ".join(rewritten)
    with path.open("w", newline="") as fh:
        writer = csv.DictWriter(fh, fieldnames=fields)
        writer.writeheader()
        writer.writerows(rows)
    return fixed, unfixable


def spec_sections(root):
    """Map every `## N. Title` / `### Subtitle` in the spec to its section number.

    Returns {(number, title)} so a `§N Title` ref is only valid when that title
    really lives under that numbered section.
    """
    text = (root / "docs/product-design.md").read_text(encoding="utf-8")
    sections, current = set(), None
    for line in text.splitlines():
        m = HEADING.match(line)
        if not m:
            continue
        if m["hashes"] == "##" and m["number"]:
            current = m["number"]
            sections.add((current, m["title"]))
        elif m["hashes"] == "###" and current:
            sections.add((current, m["title"]))
    return sections


def check_spec_ref(sections, ref):
    """Return an error string, or None when the spec reference resolves."""
    if ref == "-":
        return None
    m = SPEC_REF.match(ref)
    if not m:
        return f"malformed spec_ref {ref!r} (want '§N Heading' or '-')"
    if (m["number"], m["heading"]) not in sections:
        return f"spec_ref {ref!r} names no heading under section {m['number']}"
    return None


def main():
    args = [a for a in sys.argv[1:] if a != "--fix"]
    root = pathlib.Path(args[0] if args else ".").resolve()
    sections = spec_sections(root)
    path = root / "docs/qa/feature-inventory.csv"
    if "--fix" in sys.argv[1:]:
        fixed, unfixable = fix_anchors(root, path)
        print(f"re-anchored {fixed} citation(s)")
        for u in unfixable:
            print("  unfixable " + u)
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
        for col in ("feature_name", "user_story", "expected_behaviour", "how_to_test",
                    "spec_ref", "code_ref"):
            if not r[col].strip():
                errors.append(f"{fid}: empty {col}")
        for ref in (x.strip() for x in r["spec_ref"].split(";")):
            if not ref:
                continue
            problem = check_spec_ref(sections, ref)
            if problem:
                errors.append(f"{fid}: {problem}")
        if r["status"] not in VALID_STATUS:
            errors.append(f"{fid}: status {r['status']!r} not in {sorted(VALID_STATUS)}")
        if r["retest_status"] not in VALID_RETEST:
            errors.append(f"{fid}: retest_status {r['retest_status']!r} not in {sorted(VALID_RETEST)}")
        for col in ("expected_behaviour", "how_to_test"):
            for hit in PROSE_LINE_REF.findall(r[col]):
                errors.append(f"{fid}: unverifiable line citation {hit.strip()!r} "
                              f"in {col}; put the anchor in code_ref")
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
