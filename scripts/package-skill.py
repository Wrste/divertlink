#!/usr/bin/env python3
"""Build a portable DivertLink deployment skill ZIP (Python 3.9+, stdlib)."""
import argparse
import hashlib
from pathlib import Path
import tempfile
import zipfile

ROOT = Path(__file__).resolve().parent.parent
SKILL = ROOT / "skills" / "divertlink-deploy"


def package(output):
    output = Path(output).absolute()
    files = []
    # Explicit allow-list: exclude caches, local secrets and test output by construction.
    for pattern in ("SKILL.md", "agents/*.yaml", "references/*.md", "scripts/*.py"):
        for source in sorted(SKILL.glob(pattern)):
            if source.is_symlink() or not source.is_file():
                raise ValueError("Skill package inputs must be regular files")
            files.append((source, "divertlink-deploy/" + source.relative_to(SKILL).as_posix()))
    for license_name in ("LICENSE-MIT", "LICENSE-APACHE"):
        files.append((ROOT / license_name, "divertlink-deploy/" + license_name))
    if not (SKILL / "SKILL.md").is_file():
        raise ValueError("Missing SKILL.md")
    output.parent.mkdir(parents=True, exist_ok=True)
    with tempfile.TemporaryDirectory(prefix=".skill-package-", dir=output.parent) as temp:
        staged = Path(temp) / "skill.zip"
        with zipfile.ZipFile(staged, "w", compression=zipfile.ZIP_DEFLATED) as archive:
            for source, name in sorted(files, key=lambda item: item[1]):
                info = zipfile.ZipInfo(name, date_time=(2026, 1, 1, 0, 0, 0))
                info.compress_type = zipfile.ZIP_DEFLATED
                info.create_system = 3
                mode = 0o755 if "/scripts/" in name else 0o644
                info.external_attr = (0o100000 | mode) << 16
                archive.writestr(info, source.read_bytes())
        staged.replace(output)
    checksum = hashlib.sha256(output.read_bytes()).hexdigest()
    output.with_name(output.name + ".sha256").write_text(checksum + "  " + output.name + "\n", encoding="ascii")
    return output


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--output", type=Path, default=ROOT / "dist" / "divertlink-deploy-skill.zip")
    args = parser.parse_args()
    print(package(args.output))


if __name__ == "__main__":
    main()
