import argparse
import os
import re
import shutil
import subprocess
import sys
from pathlib import Path

NAME = "Benjamin Kleyner"
EMAIL = "54718380+benkleyner@users.noreply.github.com"
TAP = f"local/termleaf-bump-{os.getpid()}"


def checksums(path):
    sums = {}
    for line in Path(path).read_text().splitlines():
        m = re.fullmatch(r"([0-9a-f]{64}) [ *]?(\S+)", line.strip())
        if m:
            sums[m[2]] = m[1]
    return sums


def bump(text, tag, sums):
    out, pending = [], None
    for line in text.splitlines():
        if m := re.fullmatch(r'(\s*)version "[^"]*"\s*', line):
            line = f'{m[1]}version "{tag[1:]}"'
        elif m := re.fullmatch(r'(\s*)url "(.*/releases/download/)[^/"]+/([^/"]+)"\s*', line):
            if pending:
                sys.exit(f"url for {pending} has no sha256")
            pending = m[3]
            if pending not in sums:
                sys.exit(f"no checksum for {pending}")
            line = f'{m[1]}url "{m[2]}{tag}/{pending}"'
        elif re.match(r"\s*url ", line):
            sys.exit(f"unexpected url line: {line.strip()}")
        elif m := re.fullmatch(r'(\s*)sha256 "[0-9a-f]*"\s*', line):
            if not pending:
                sys.exit("sha256 without a url")
            line = f'{m[1]}sha256 "{sums[pending]}"'
            pending = None
        out.append(line)
    if pending:
        sys.exit(f"url for {pending} has no sha256")
    return "\n".join(out) + "\n"


def brew(*args, env):
    print("$ brew " + " ".join(args), flush=True)
    return subprocess.run(["brew", *args], env=env).returncode == 0


def check(formula):
    env = os.environ | {"HOMEBREW_NO_AUTO_UPDATE": "1", "HOMEBREW_NO_ANALYTICS": "1", "HOMEBREW_NO_ENV_HINTS": "1"}
    subprocess.run(["brew", "tap-new", "--no-git", TAP], env=env, check=True, stdout=subprocess.DEVNULL)
    tap = Path(subprocess.run(["brew", "--repository", TAP], env=env, check=True, capture_output=True, text=True).stdout.strip())
    try:
        shutil.copy(formula, tap / "Formula" / formula.name)
        name = f"{TAP}/{formula.stem}"
        return brew("audit", "--strict", name, env=env) & brew("style", name, env=env)
    finally:
        shutil.rmtree(tap)


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--dry-run", action="store_true")
    parser.add_argument("tag")
    parser.add_argument("formula", type=Path)
    parser.add_argument("checksums")
    args = parser.parse_args()
    if not re.fullmatch(r"v\d+\.\d+\.\d+", args.tag):
        sys.exit(f"not a release tag: {args.tag}")
    old = args.formula.read_text()
    new = bump(old, args.tag, checksums(args.checksums))
    if new == old:
        print("formula already current")
        return
    args.formula.write_text(new)
    print(f"updated {args.formula} to {args.tag}")
    if not check(args.formula):
        sys.exit("brew audit or brew style failed")
    if args.dry_run:
        print("dry run: not committing")
        return
    repo = ["git", "-C", str(args.formula.parent)]
    subprocess.run([*repo, "add", args.formula.name], check=True)
    subprocess.run([*repo, "-c", f"user.name={NAME}", "-c", f"user.email={EMAIL}", "commit", "-m", f"termleaf {args.tag[1:]}"], check=True)
    subprocess.run([*repo, "push"], check=True)


if __name__ == "__main__":
    main()
