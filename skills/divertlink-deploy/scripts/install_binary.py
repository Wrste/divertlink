#!/usr/bin/env python3
"""Install a checksum-verified DivertLink release. Python 3.9+, standard library."""
import argparse
import hashlib
import io
import json
import os
from pathlib import Path
import platform
import re
import subprocess
import sys
import tarfile
import tempfile
import urllib.request
import zipfile


class InstallError(ValueError):
    """A sanitized error suitable for public output."""


RELEASES = "https://github.com/Wrste/divertlink/releases/download"
MAX_ARCHIVE = 128 * 1024 * 1024


def target_for(system, machine):
    machine = machine.lower()
    arch = {"amd64": "x86_64", "x86_64": "x86_64", "aarch64": "aarch64", "arm64": "aarch64"}.get(machine)
    targets = {"Linux": "unknown-linux-musl", "Darwin": "apple-darwin", "Windows": "pc-windows-msvc"}
    if not arch or system not in targets or (system == "Windows" and arch != "x86_64"):
        raise InstallError("No prebuilt release for this OS/CPU; use the source build instructions")
    return arch + "-" + targets[system]


def download(url, limit):
    request = urllib.request.Request(url, headers={"User-Agent": "DivertLink-deploy-skill"})
    with urllib.request.urlopen(request, timeout=60) as response:
        data = response.read(limit + 1)
    if len(data) > limit:
        raise InstallError("Release download exceeds size limit")
    return data


def unpack_binary(data, name, windows):
    executable = "divertlink.exe" if windows else "divertlink"
    member_name = name + "/" + executable
    # Read exactly one regular file into memory; never extract arbitrary archive paths.
    if windows:
        with zipfile.ZipFile(io.BytesIO(data)) as archive:
            members = [x for x in archive.infolist() if x.filename == member_name]
            info = members[0] if len(members) == 1 else None
            mode = (info.external_attr >> 16) & 0o170000 if info else 0
            if (info is None or info.is_dir() or info.file_size > MAX_ARCHIVE
                    or mode in (0o120000, 0o060000)):
                raise InstallError("Unexpected executable in release ZIP")
            return archive.read(info)
    with tarfile.open(fileobj=io.BytesIO(data), mode="r:gz") as archive:
        members = [x for x in archive.getmembers() if x.name == member_name]
        if len(members) != 1 or not members[0].isreg() or members[0].size > MAX_ARCHIVE:
            raise InstallError("Unexpected executable in release archive")
        with archive.extractfile(members[0]) as handle:
            return handle.read()


def install(version, bin_dir, replace=False, dry_run=False):
    version = version.removeprefix("v")
    if not re.fullmatch(r"\d+\.\d+\.\d+(?:-[0-9A-Za-z.-]+)?", version):
        raise InstallError("Version must be a published semver such as 0.1.0")
    system = platform.system()
    target = target_for(system, platform.machine())
    name = "divertlink-" + version + "-" + target
    extension = ".zip" if system == "Windows" else ".tar.gz"
    url = RELEASES + "/v" + version + "/" + name + extension
    destination = Path(bin_dir).expanduser().absolute() / ("divertlink.exe" if system == "Windows" else "divertlink")
    plan = {"version": version, "target": target, "url": url, "binary": str(destination)}
    if dry_run:
        return dict(plan, dry_run=True)
    if destination.exists() and not replace:
        raise InstallError("Binary exists; stop its processes and use --replace for an intended upgrade")
    checksum = download(url + ".sha256", 2048).decode("ascii").strip().split()
    if len(checksum) != 2 or not re.fullmatch(r"[a-fA-F0-9]{64}", checksum[0]) or checksum[1].lstrip("*") != name + extension:
        raise InstallError("Malformed or mismatched checksum file")
    data = download(url, MAX_ARCHIVE)
    digest = hashlib.sha256(data).hexdigest()
    if digest != checksum[0].lower():
        raise InstallError("SHA256 mismatch; existing binary was not modified")
    binary_data = unpack_binary(data, name, system == "Windows")
    destination.parent.mkdir(parents=True, exist_ok=True)
    # Stage in the destination filesystem, verify, then atomically replace the final executable.
    with tempfile.TemporaryDirectory(prefix=".divertlink-install-", dir=destination.parent) as temp:
        staged = Path(temp) / destination.name
        staged.write_bytes(binary_data)
        staged.chmod(0o755)
        if system == "Darwin":
            result = subprocess.run(["codesign", "--verify", "--strict", str(staged)], capture_output=True)
            if result.returncode:
                raise InstallError("macOS code signature verification failed")
        result = subprocess.run([str(staged), "version"], capture_output=True, text=True, timeout=15)
        if result.returncode or result.stdout.splitlines()[0:1] != ["divertlink " + version]:
            raise InstallError("Downloaded binary failed version verification")
        if replace:
            os.replace(staged, destination)
        else:
            # A concurrent installer must not silently overwrite an existing executable.
            os.link(staged, destination)
    return dict(plan, sha256=digest, installed=True)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--version", required=True, help="Published release, e.g. 0.1.0")
    parser.add_argument("--bin-dir", required=True, help="User-writable binary directory")
    parser.add_argument("--replace", action="store_true", help="Replace binary only; never changes configs/services")
    parser.add_argument("--dry-run", action="store_true", help="Print plan; no downloads or writes")
    args = parser.parse_args()
    try:
        print(json.dumps(install(args.version, args.bin_dir, args.replace, args.dry_run), ensure_ascii=False))
    except Exception as error:
        # Avoid HTTP/proxy error text that might include environment credentials.
        message = str(error) if isinstance(error, InstallError) else type(error).__name__
        print(json.dumps({"ok": False, "error": message}), file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
