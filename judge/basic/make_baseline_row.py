#!/usr/bin/env python3
import argparse
import datetime as dt
import re
import subprocess
import sys


def percent(passed: int, total: int) -> str:
    if total == 0:
        return "0.0%"
    return f"{(passed * 100.0 / total):.1f}%"


def git_short_rev(cwd: str) -> str:
    try:
        out = subprocess.check_output(
            ["git", "rev-parse", "--short", "HEAD"], cwd=cwd, text=True
        ).strip()
        return out or "-"
    except Exception:
        return "-"


def main() -> int:
    parser = argparse.ArgumentParser(
        description="Generate a markdown baseline row from batch_judge output."
    )
    parser.add_argument("--log", required=True, help="Path to judge output log")
    parser.add_argument(
        "--repo", default=".", help="Repo path used to get git short revision"
    )
    parser.add_argument("--note", default="", help="Optional note column")
    args = parser.parse_args()

    txt = open(args.log, encoding="utf-8", errors="ignore").read()
    pat = re.compile(r"\[(basic-musl|basic-glibc)\]\s+checks:\s+(\d+)/(\d+)")
    found = {m.group(1): (int(m.group(2)), int(m.group(3))) for m in pat.finditer(txt)}

    musl = found.get("basic-musl", (0, 0))
    glibc = found.get("basic-glibc", (0, 0))
    today = dt.date.today().isoformat()
    rev = git_short_rev(args.repo)

    row = (
        f"| {today} | {rev} | {musl[0]}/{musl[1]} ({percent(*musl)}) | "
        f"{glibc[0]}/{glibc[1]} ({percent(*glibc)}) | {args.note} |"
    )
    print(row)
    return 0


if __name__ == "__main__":
    sys.exit(main())
