"""Install a pinned standalone release into an Action-owned directory."""

import hashlib
import platform
from pathlib import Path
import re
import shutil
import stat
import subprocess
import tarfile
import urllib.request
import zipfile


def version_string(value):
    if not re.fullmatch(r"(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)", value):
        raise ValueError("version must be an explicit stable MAJOR.MINOR.PATCH without v")
    return value


def release_target(system=None, machine=None, libc=None):
    system = system or platform.system()
    machine = (machine or platform.machine()).lower()
    arch = {"amd64": "x86_64", "x86_64": "x86_64", "arm64": "aarch64",
            "aarch64": "aarch64"}.get(machine)
    if system == "Linux" and (libc if libc is not None else platform.libc_ver()[0]) != "glibc":
        raise ValueError("Linux downloads require glibc (manylinux_2_17); musl is not published")
    suffix = {"Linux": "unknown-linux-gnu", "Darwin": "apple-darwin",
              "Windows": "pc-windows-msvc"}.get(system)
    if not arch or not suffix or (system == "Windows" and arch != "x86_64"):
        raise ValueError(f"unsupported release platform: {system!r} / {machine!r}")
    return f"{arch}-{suffix}"


def verify_binary(binary, version):
    binary = Path(binary).resolve(strict=True)
    if not binary.is_file():
        raise ValueError("binary override must be an executable file")
    result = subprocess.run([str(binary), "--version"], capture_output=True, check=True,
                            timeout=30)
    if result.stdout.strip() != f"urmare {version}".encode():
        raise ValueError(f"binary must report exactly urmare {version}")
    return binary


def download(url, destination):
    with urllib.request.urlopen(url, timeout=60) as response, destination.open("xb") as out:
        shutil.copyfileobj(response, out)


def verify_checksum(archive, sums):
    matches = []
    for line in sums.read_text(encoding="utf-8").splitlines():
        match = re.fullmatch(r"([0-9a-f]{64})  ([A-Za-z0-9_.-]+)", line)
        if not match:
            raise ValueError("malformed release SHA256SUMS")
        if match[2] == archive.name:
            matches.append(match[1])
    digest = hashlib.sha256()
    with archive.open("rb") as handle:
        for chunk in iter(lambda: handle.read(1024 * 1024), b""):
            digest.update(chunk)
    if len(matches) != 1 or digest.hexdigest() != matches[0]:
        raise ValueError(f"SHA256SUMS mismatch or missing/duplicate entry for {archive.name}")


def extract_binary(archive, directory, package_root, executable):
    """Validate all members, then copy only the regular executable; never extractall."""
    expected = f"{package_root}/{executable}"
    seen = set()

    def safe(name, regular, is_directory):
        parts = name.rstrip("/").split("/")
        if (name in seen or "\\" in name or ":" in name or name.startswith("/")
                or any(part in ("", ".", "..") for part in parts)
                or any(ord(c) < 32 for c in name) or parts[0] != package_root
                or not (regular or is_directory)):
            raise ValueError(f"unsafe archive member: {name!r}")
        seen.add(name)

    binary = directory / executable
    if archive.suffix == ".zip":
        with zipfile.ZipFile(archive) as handle:
            for member in handle.infolist():
                mode = member.external_attr >> 16
                safe(member.filename, stat.S_ISREG(mode), member.is_dir() and stat.S_ISDIR(mode))
            if expected not in seen:
                raise ValueError("archive is missing the expected executable")
            with handle.open(expected) as source, binary.open("xb") as dest:
                shutil.copyfileobj(source, dest)
    else:
        with tarfile.open(archive, "r:gz") as handle:
            for member in handle.getmembers():
                safe(member.name, member.isfile(), member.isdir())
            member = handle.getmember(expected) if expected in seen else None
            if member is None or not member.isfile():
                raise ValueError("archive is missing the expected regular executable")
            with handle.extractfile(member) as source, binary.open("xb") as dest:
                shutil.copyfileobj(source, dest)
    binary.chmod(0o755)
    return binary


def install(version, override, directory, workspace):
    version_string(version)  # Validate before any URL construction, even with override.
    if override:
        path = Path(override)
        return verify_binary(path if path.is_absolute() else workspace / path, version)
    target = release_target()
    package_root = f"urmare-v{version}-{target}"
    extension = ".zip" if target.endswith("windows-msvc") else ".tar.gz"
    archive = directory / (package_root + extension)
    sums = directory / "SHA256SUMS"
    base = f"https://github.com/sorginte/urmare/releases/download/v{version}"
    download(base + "/SHA256SUMS", sums)
    download(base + "/" + archive.name, archive)
    verify_checksum(archive, sums)
    try:
        binary = extract_binary(archive, directory, package_root,
                                "urmare.exe" if extension == ".zip" else "urmare")
    except (tarfile.TarError, zipfile.BadZipFile) as error:
        raise ValueError(f"invalid release archive: {error}") from error
    return verify_binary(binary, version)
