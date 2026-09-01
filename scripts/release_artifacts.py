#!/usr/bin/env python3
"""Build-independent validation and packaging for Urmare release artifacts."""

from __future__ import annotations

import argparse
import email.parser
import gzip
import hashlib
import json
import os
import re
import stat
import subprocess
import sys
import tarfile
import tempfile
import venv
import zipfile
from pathlib import Path
from typing import Dict, List, Mapping, Optional, Sequence, Tuple


PACKAGE = "urmare"
TARGETS = (
    "aarch64-apple-darwin",
    "x86_64-apple-darwin",
    "aarch64-unknown-linux-gnu",
    "x86_64-unknown-linux-gnu",
    "x86_64-pc-windows-msvc",
)
TAG_PATTERN = re.compile(r"^v(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)$")
SHA_PATTERN = re.compile(r"^[0-9a-f]{40}$")


class ArtifactError(RuntimeError):
    """An artifact violated the release contract."""


def sha256_bytes(contents: bytes) -> str:
    return hashlib.sha256(contents).hexdigest()


def sha256_file(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as handle:
        for chunk in iter(lambda: handle.read(1024 * 1024), b""):
            digest.update(chunk)
    return digest.hexdigest()


def workspace_version(cargo_toml: Path) -> str:
    in_workspace_package = False
    for raw_line in cargo_toml.read_text(encoding="utf-8").splitlines():
        line = raw_line.strip()
        if line.startswith("["):
            in_workspace_package = line == "[workspace.package]"
            continue
        if in_workspace_package:
            match = re.fullmatch(r'version\s*=\s*"([^"]+)"', line)
            if match:
                return match.group(1)
    raise ArtifactError(
        f"could not read workspace.package.version from {cargo_toml}"
    )


def validate_release_identity(tag: str, version: str, commit_sha: str) -> None:
    if not TAG_PATTERN.fullmatch(tag):
        raise ArtifactError(
            f"release tag must be a stable semantic version such as v0.1.0; got {tag!r}"
        )
    if tag[1:] != version:
        raise ArtifactError(
            f"release tag {tag!r} does not match Cargo workspace version {version!r}"
        )
    if not SHA_PATTERN.fullmatch(commit_sha):
        raise ArtifactError(
            f"release commit must be a full lowercase 40-character Git SHA; got {commit_sha!r}"
        )


def one_file(directory: Path, pattern: str, kind: str) -> Path:
    matches = sorted(path for path in directory.glob(pattern) if path.is_file())
    if len(matches) != 1:
        names = ", ".join(path.name for path in matches) or "none"
        raise ArtifactError(
            f"expected exactly one {kind} in {directory}, found {len(matches)}: {names}"
        )
    return matches[0]


def parse_wheel_filename(path: Path) -> Tuple[str, str, str, str, str]:
    if path.suffix != ".whl":
        raise ArtifactError(f"expected a .whl file, got {path.name!r}")
    parts = path.name[:-4].split("-")
    if len(parts) != 5:
        raise ArtifactError(
            f"wheel filename must not contain a build tag and must have five components: {path.name}"
        )
    return tuple(parts)  # type: ignore[return-value]


def expected_archive_name(tag: str, target: str) -> str:
    extension = "zip" if target == "x86_64-pc-windows-msvc" else "tar.gz"
    return f"{PACKAGE}-{tag}-{target}.{extension}"


def validate_platform_tag(platform_tag: str, target: str) -> None:
    tags = platform_tag.split(".")
    if target == "aarch64-apple-darwin":
        valid = any(re.fullmatch(r"macosx_[0-9]+_[0-9]+_arm64", tag) for tag in tags)
    elif target == "x86_64-apple-darwin":
        valid = any(re.fullmatch(r"macosx_[0-9]+_[0-9]+_x86_64", tag) for tag in tags)
    elif target == "aarch64-unknown-linux-gnu":
        valid = "manylinux_2_17_aarch64" in tags
    elif target == "x86_64-unknown-linux-gnu":
        valid = "manylinux_2_17_x86_64" in tags
    elif target == "x86_64-pc-windows-msvc":
        valid = tags == ["win_amd64"]
    else:
        raise ArtifactError(f"unsupported release target {target!r}")
    if not valid:
        raise ArtifactError(
            f"wheel platform tag {platform_tag!r} does not match target {target!r}"
        )


def expanded_compatibility_tags(
    python_tag: str, abi_tag: str, platform_tag: str
) -> List[str]:
    """Expand the compressed compatibility tag sets used in a wheel filename."""
    components = (python_tag.split("."), abi_tag.split("."), platform_tag.split("."))
    if any(not value for values in components for value in values):
        raise ArtifactError("wheel filename contains an empty compatibility tag")
    return sorted(
        f"{python_value}-{abi_value}-{platform_value}"
        for python_value in components[0]
        for abi_value in components[1]
        for platform_value in components[2]
    )


def parse_metadata(contents: bytes) -> Mapping[str, str]:
    return email.parser.BytesParser().parsebytes(contents)


def validate_wheel(path: Path, target: str, version: str) -> Tuple[str, bytes]:
    distribution, filename_version, python_tag, abi_tag, platform_tag = (
        parse_wheel_filename(path)
    )
    if distribution.replace("_", "-").lower() != PACKAGE:
        raise ArtifactError(
            f"wheel distribution must be {PACKAGE!r}, got {distribution!r}"
        )
    if filename_version != version:
        raise ArtifactError(
            f"wheel filename version {filename_version!r} does not match {version!r}"
        )
    if python_tag != "py3" or abi_tag != "none":
        raise ArtifactError(
            f"wheel must be interpreter-independent (py3-none), got {python_tag}-{abi_tag}"
        )
    validate_platform_tag(platform_tag, target)

    executable = "urmare.exe" if target == "x86_64-pc-windows-msvc" else "urmare"
    with zipfile.ZipFile(path) as wheel:
        names = wheel.namelist()
        metadata_names = [name for name in names if name.endswith(".dist-info/METADATA")]
        wheel_names = [name for name in names if name.endswith(".dist-info/WHEEL")]
        script_names = [
            name
            for name in names
            if name.endswith(f".data/scripts/{executable}")
        ]
        if len(metadata_names) != 1 or len(wheel_names) != 1 or len(script_names) != 1:
            raise ArtifactError(
                f"wheel {path.name} must contain exactly one METADATA, WHEEL, and {executable} script"
            )
        metadata = parse_metadata(wheel.read(metadata_names[0]))
        if metadata.get("Name", "").lower() != PACKAGE:
            raise ArtifactError(
                f"wheel METADATA Name must be {PACKAGE!r}, got {metadata.get('Name')!r}"
            )
        if metadata.get("Version") != version:
            raise ArtifactError(
                f"wheel METADATA Version {metadata.get('Version')!r} does not match {version!r}"
            )
        if metadata.get("Requires-Python") != ">=3.9":
            raise ArtifactError(
                "wheel METADATA Requires-Python must be exactly '>=3.9'"
            )
        wheel_metadata = parse_metadata(wheel.read(wheel_names[0]))
        wheel_tags = wheel_metadata.get_all("Tag", [])
        expected_tags = expanded_compatibility_tags(
            python_tag, abi_tag, platform_tag
        )
        if sorted(wheel_tags) != expected_tags:
            raise ArtifactError(
                "wheel WHEEL metadata tags do not match the expanded filename tags; "
                f"expected {expected_tags!r}, got {wheel_tags!r}"
            )
        return script_names[0], wheel.read(script_names[0])


def tar_info(name: str, contents: Optional[bytes], mode: int) -> tarfile.TarInfo:
    info = tarfile.TarInfo(name)
    info.uid = 0
    info.gid = 0
    info.uname = ""
    info.gname = ""
    info.mtime = 0
    info.mode = mode
    if contents is None:
        info.type = tarfile.DIRTYPE
        info.size = 0
    else:
        info.size = len(contents)
    return info


def write_tar_archive(
    path: Path, package_root: str, files: Sequence[Tuple[str, bytes, int]]
) -> None:
    import io

    with path.open("wb") as raw:
        with gzip.GzipFile(filename="", mode="wb", fileobj=raw, mtime=0) as compressed:
            with tarfile.open(fileobj=compressed, mode="w", format=tarfile.GNU_FORMAT) as archive:
                archive.addfile(tar_info(f"{package_root}/", None, 0o755))
                for name, contents, mode in files:
                    archive.addfile(
                        tar_info(f"{package_root}/{name}", contents, mode),
                        io.BytesIO(contents),
                    )


def write_zip_archive(
    path: Path, package_root: str, files: Sequence[Tuple[str, bytes, int]]
) -> None:
    with zipfile.ZipFile(path, "w", compression=zipfile.ZIP_DEFLATED, compresslevel=9) as archive:
        for name, contents, mode in files:
            info = zipfile.ZipInfo(f"{package_root}/{name}", (1980, 1, 1, 0, 0, 0))
            info.compress_type = zipfile.ZIP_DEFLATED
            info.create_system = 3
            info.external_attr = (stat.S_IFREG | mode) << 16
            archive.writestr(info, contents)


def archive_binary(path: Path, package_root: str, executable: str) -> bytes:
    member_name = f"{package_root}/{executable}"
    if path.suffix == ".zip":
        with zipfile.ZipFile(path) as archive:
            names = archive.namelist()
            if names.count(member_name) != 1:
                raise ArtifactError(
                    f"archive {path.name} must contain exactly one {member_name}"
                )
            return archive.read(member_name)
    with tarfile.open(path, "r:gz") as archive:
        members = [member for member in archive.getmembers() if member.name == member_name]
        if len(members) != 1:
            raise ArtifactError(
                f"archive {path.name} must contain exactly one {member_name}"
            )
        handle = archive.extractfile(members[0])
        if handle is None:
            raise ArtifactError(f"archive member {member_name} is not a regular file")
        return handle.read()


def package_target(args: argparse.Namespace) -> None:
    target = args.target
    if target not in TARGETS:
        raise ArtifactError(f"unsupported release target {target!r}")
    validate_release_identity(args.tag, args.version, args.commit)
    cargo_version = workspace_version(args.repository / "Cargo.toml")
    if cargo_version != args.version:
        raise ArtifactError(
            f"Cargo workspace version {cargo_version!r} does not match requested version {args.version!r}"
        )

    wheel = one_file(args.wheel_dir, "*.whl", "wheel")
    _, binary = validate_wheel(wheel, target, args.version)
    executable = "urmare.exe" if target == "x86_64-pc-windows-msvc" else "urmare"
    package_root = f"{PACKAGE}-{args.tag}-{target}"
    archive_name = expected_archive_name(args.tag, target)
    args.archive_dir.mkdir(parents=True, exist_ok=True)
    args.manifest_dir.mkdir(parents=True, exist_ok=True)
    archive = args.archive_dir / archive_name
    if archive.exists():
        raise ArtifactError(f"refusing to overwrite existing archive {archive}")
    files = (
        (executable, binary, 0o755),
        ("LICENSE", (args.repository / "LICENSE").read_bytes(), 0o644),
        ("README.md", (args.repository / "README.md").read_bytes(), 0o644),
    )
    if target == "x86_64-pc-windows-msvc":
        write_zip_archive(archive, package_root, files)
    else:
        write_tar_archive(archive, package_root, files)

    archived_binary = archive_binary(archive, package_root, executable)
    binary_digest = sha256_bytes(binary)
    if sha256_bytes(archived_binary) != binary_digest:
        raise ArtifactError(
            f"binary identity check failed between {wheel.name} and {archive.name}"
        )

    manifest = {
        "schema_version": 1,
        "release_tag": args.tag,
        "version": args.version,
        "commit_sha": args.commit,
        "target": target,
        "archive": {
            "filename": archive.name,
            "sha256": sha256_file(archive),
        },
        "wheel": {
            "filename": wheel.name,
            "sha256": sha256_file(wheel),
        },
        "binary_sha256": binary_digest,
    }
    manifest_path = args.manifest_dir / f"manifest-{target}.json"
    if manifest_path.exists():
        raise ArtifactError(f"refusing to overwrite existing manifest {manifest_path}")
    manifest_path.write_text(
        json.dumps(manifest, indent=2, sort_keys=True) + "\n", encoding="utf-8"
    )
    print(
        f"packaged {archive.name} from {wheel.name}; binary sha256={binary_digest}"
    )


def safe_extract_archive(archive: Path, destination: Path) -> None:
    destination = destination.resolve()
    if archive.suffix == ".zip":
        with zipfile.ZipFile(archive) as handle:
            for member in handle.infolist():
                output = (destination / member.filename).resolve()
                if destination not in output.parents and output != destination:
                    raise ArtifactError(
                        f"archive member escapes extraction root: {member.filename}"
                    )
            handle.extractall(destination)
        return
    with tarfile.open(archive, "r:gz") as handle:
        for member in handle.getmembers():
            output = (destination / member.name).resolve()
            if destination not in output.parents and output != destination:
                raise ArtifactError(
                    f"archive member escapes extraction root: {member.name}"
                )
            if member.issym() or member.islnk():
                raise ArtifactError(f"release archive contains a link: {member.name}")
        handle.extractall(destination)


def run_checked(command: Sequence[str], expected_fragment: Optional[str] = None) -> str:
    completed = subprocess.run(
        command,
        check=False,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        text=True,
    )
    if completed.returncode != 0:
        raise ArtifactError(
            f"command failed with exit {completed.returncode}: {command!r}\n"
            f"stdout:\n{completed.stdout}\nstderr:\n{completed.stderr}"
        )
    if expected_fragment is not None and expected_fragment not in completed.stdout:
        raise ArtifactError(
            f"command {command!r} output did not contain {expected_fragment!r}:\n{completed.stdout}"
        )
    return completed.stdout


def smoke_binary(binary: Path, version: str, fixture: Path) -> None:
    run_checked([str(binary), "--version"], f"urmare {version}")
    run_checked([str(binary), "--help"], "Usage: urmare")
    run_checked(
        [
            str(binary),
            "--root",
            str(fixture),
            "impact",
            "package/foo.py",
        ],
        "Impact analysis",
    )


def venv_executable(environment: Path, name: str) -> Path:
    if os.name == "nt":
        return environment / "Scripts" / f"{name}.exe"
    return environment / "bin" / name


def smoke_wheel(wheel: Path, target: str, version: str, fixture: Path) -> None:
    _, wheel_binary = validate_wheel(wheel, target, version)
    with tempfile.TemporaryDirectory(prefix="urmare-wheel-") as temporary:
        environment = Path(temporary) / "venv"
        venv.EnvBuilder(with_pip=True, clear=True).create(environment)
        python = venv_executable(environment, "python")
        pip_environment = os.environ.copy()
        pip_environment.update(
            {
                "PIP_DISABLE_PIP_VERSION_CHECK": "1",
                "PIP_NO_INDEX": "1",
                "PIP_ONLY_BINARY": ":all:",
            }
        )
        completed = subprocess.run(
            [
                str(python),
                "-m",
                "pip",
                "install",
                "--no-deps",
                "--force-reinstall",
                str(wheel),
            ],
            check=False,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            text=True,
            env=pip_environment,
        )
        if completed.returncode != 0:
            raise ArtifactError(
                f"isolated wheel installation failed:\n{completed.stdout}\n{completed.stderr}"
            )
        binary = venv_executable(environment, "urmare")
        if not binary.is_file():
            raise ArtifactError(f"wheel did not install the urmare command at {binary}")
        if sha256_file(binary) != sha256_bytes(wheel_binary):
            raise ArtifactError(
                "installed urmare binary does not match the binary stored in the wheel"
            )
        smoke_binary(binary, version, fixture)


def smoke_target(args: argparse.Namespace) -> None:
    wheel = one_file(args.wheel_dir, "*.whl", "wheel")
    archive = one_file(args.archive_dir, "*", "standalone archive")
    expected_name = expected_archive_name(args.tag, args.target)
    if archive.name != expected_name:
        raise ArtifactError(
            f"expected standalone archive {expected_name!r}, got {archive.name!r}"
        )
    package_root = f"{PACKAGE}-{args.tag}-{args.target}"
    executable = "urmare.exe" if args.target == "x86_64-pc-windows-msvc" else "urmare"
    _, wheel_binary = validate_wheel(wheel, args.target, args.version)
    if sha256_bytes(archive_binary(archive, package_root, executable)) != sha256_bytes(
        wheel_binary
    ):
        raise ArtifactError("wheel/archive binary identity check failed during smoke test")
    with tempfile.TemporaryDirectory(prefix="urmare-archive-") as temporary:
        extracted = Path(temporary)
        safe_extract_archive(archive, extracted)
        binary = extracted / package_root / executable
        if os.name != "nt":
            binary.chmod(binary.stat().st_mode | stat.S_IXUSR)
        smoke_binary(binary, args.version, args.fixture)
    smoke_wheel(wheel, args.target, args.version, args.fixture)


def smoke_wheel_command(args: argparse.Namespace) -> None:
    wheel = one_file(args.wheel_dir, "*.whl", "wheel")
    smoke_wheel(wheel, args.target, args.version, args.fixture)


def load_manifest(path: Path) -> Dict[str, object]:
    try:
        value = json.loads(path.read_text(encoding="utf-8"))
    except (OSError, json.JSONDecodeError) as error:
        raise ArtifactError(f"unable to read manifest {path}: {error}") from error
    if not isinstance(value, dict):
        raise ArtifactError(f"manifest {path} must contain a JSON object")
    return value


def exact_files(directory: Path) -> List[Path]:
    return sorted(path for path in directory.iterdir() if path.is_file())


def verify_release_set(args: argparse.Namespace) -> None:
    validate_release_identity(args.tag, args.version, args.commit)
    archives = exact_files(args.archives_dir)
    wheels = exact_files(args.wheels_dir)
    manifest_paths = exact_files(args.manifests_dir)
    if len(wheels) != len(TARGETS) or any(path.suffix != ".whl" for path in wheels):
        raise ArtifactError("wheel channel must contain exactly five .whl files and no sdist")
    expected_archives = {expected_archive_name(args.tag, target) for target in TARGETS}
    if {path.name for path in archives} != expected_archives:
        raise ArtifactError(
            "standalone archive set mismatch; expected exactly: "
            + ", ".join(sorted(expected_archives))
        )
    if len(manifest_paths) != len(TARGETS):
        raise ArtifactError("manifest-part channel must contain exactly five JSON files")

    manifests_by_target: Dict[str, Dict[str, object]] = {}
    for manifest_path in manifest_paths:
        manifest = load_manifest(manifest_path)
        target = manifest.get("target")
        if target not in TARGETS or not isinstance(target, str):
            raise ArtifactError(f"manifest {manifest_path} has unsupported target {target!r}")
        if target in manifests_by_target:
            raise ArtifactError(f"duplicate target manifest for {target}")
        for key, expected in (
            ("release_tag", args.tag),
            ("version", args.version),
            ("commit_sha", args.commit),
        ):
            if manifest.get(key) != expected:
                raise ArtifactError(
                    f"manifest {manifest_path} {key}={manifest.get(key)!r}, expected {expected!r}"
                )
        manifests_by_target[target] = manifest

    wheel_by_name = {path.name: path for path in wheels}
    archive_by_name = {path.name: path for path in archives}
    combined: List[Dict[str, object]] = []
    for target in TARGETS:
        manifest = manifests_by_target.get(target)
        if manifest is None:
            raise ArtifactError(f"missing target manifest for {target}")
        archive_data = manifest.get("archive")
        wheel_data = manifest.get("wheel")
        if not isinstance(archive_data, dict) or not isinstance(wheel_data, dict):
            raise ArtifactError(f"manifest for {target} has invalid artifact records")
        archive_name = archive_data.get("filename")
        wheel_name = wheel_data.get("filename")
        if not isinstance(archive_name, str) or archive_name not in archive_by_name:
            raise ArtifactError(f"manifest for {target} references an unavailable archive")
        if not isinstance(wheel_name, str) or wheel_name not in wheel_by_name:
            raise ArtifactError(f"manifest for {target} references an unavailable wheel")
        archive = archive_by_name[archive_name]
        wheel = wheel_by_name[wheel_name]
        if archive_data.get("sha256") != sha256_file(archive):
            raise ArtifactError(f"archive digest mismatch for {archive.name}")
        if wheel_data.get("sha256") != sha256_file(wheel):
            raise ArtifactError(f"wheel digest mismatch for {wheel.name}")
        _, wheel_binary = validate_wheel(wheel, target, args.version)
        package_root = f"{PACKAGE}-{args.tag}-{target}"
        executable = "urmare.exe" if target == "x86_64-pc-windows-msvc" else "urmare"
        binary_digest = sha256_bytes(wheel_binary)
        if manifest.get("binary_sha256") != binary_digest:
            raise ArtifactError(f"binary digest mismatch in manifest for {target}")
        if sha256_bytes(archive_binary(archive, package_root, executable)) != binary_digest:
            raise ArtifactError(f"wheel/archive binary identity mismatch for {target}")
        combined.append(manifest)

    args.output.parent.mkdir(parents=True, exist_ok=True)
    if args.output.exists():
        raise ArtifactError(f"refusing to overwrite existing release manifest {args.output}")
    output = {
        "schema_version": 1,
        "release_tag": args.tag,
        "version": args.version,
        "commit_sha": args.commit,
        "artifacts": combined,
    }
    args.output.write_text(
        json.dumps(output, indent=2, sort_keys=True) + "\n", encoding="utf-8"
    )
    print(f"verified five wheels and five standalone archives for {args.tag}")


def prepare_release_assets(args: argparse.Namespace) -> None:
    expected = [expected_archive_name(args.tag, target) for target in TARGETS]
    actual = exact_files(args.archives_dir)
    if [path.name for path in actual] != sorted(expected):
        raise ArtifactError(
            "GitHub Release directory must contain exactly the five standalone archives"
        )
    checksums = args.archives_dir / "SHA256SUMS"
    if checksums.exists():
        raise ArtifactError(f"refusing to overwrite existing checksums file {checksums}")
    checksums.write_text(
        "".join(
            f"{sha256_file(args.archives_dir / name)}  {name}\n" for name in expected
        ),
        encoding="utf-8",
    )
    print("verified five standalone archives and wrote SHA256SUMS")


def metadata_command(args: argparse.Namespace) -> None:
    cargo_version = workspace_version(args.cargo_toml)
    version = args.version or cargo_version
    tag = args.tag or f"v{version}"
    if version != cargo_version:
        raise ArtifactError(
            f"requested version {version!r} does not match Cargo workspace version {cargo_version!r}"
        )
    validate_release_identity(tag, version, args.commit)
    values = {"version": version, "release-tag": tag, "commit-sha": args.commit}
    if args.github_output:
        with args.github_output.open("a", encoding="utf-8") as output:
            for key, value in values.items():
                output.write(f"{key}={value}\n")
    print(json.dumps(values, sort_keys=True))


def parser() -> argparse.ArgumentParser:
    root = argparse.ArgumentParser(description=__doc__)
    commands = root.add_subparsers(dest="command", required=True)

    metadata = commands.add_parser("metadata", help="validate and emit release identity")
    metadata.add_argument("--cargo-toml", type=Path, default=Path("Cargo.toml"))
    metadata.add_argument("--tag", default="")
    metadata.add_argument("--version", default="")
    metadata.add_argument("--commit", required=True)
    metadata.add_argument("--github-output", type=Path)
    metadata.set_defaults(function=metadata_command)

    package = commands.add_parser(
        "package-target", help="package a wheel's binary as a standalone archive"
    )
    package.add_argument("--repository", type=Path, default=Path("."))
    package.add_argument("--wheel-dir", type=Path, required=True)
    package.add_argument("--archive-dir", type=Path, required=True)
    package.add_argument("--manifest-dir", type=Path, required=True)
    package.add_argument("--target", required=True)
    package.add_argument("--tag", required=True)
    package.add_argument("--version", required=True)
    package.add_argument("--commit", required=True)
    package.set_defaults(function=package_target)

    smoke = commands.add_parser("smoke-target", help="execute archive and wheel installs")
    smoke.add_argument("--wheel-dir", type=Path, required=True)
    smoke.add_argument("--archive-dir", type=Path, required=True)
    smoke.add_argument("--fixture", type=Path, required=True)
    smoke.add_argument("--target", required=True)
    smoke.add_argument("--tag", required=True)
    smoke.add_argument("--version", required=True)
    smoke.set_defaults(function=smoke_target)

    wheel_smoke = commands.add_parser(
        "smoke-wheel", help="install and execute one wheel in an isolated venv"
    )
    wheel_smoke.add_argument("--wheel-dir", type=Path, required=True)
    wheel_smoke.add_argument("--fixture", type=Path, required=True)
    wheel_smoke.add_argument("--target", required=True)
    wheel_smoke.add_argument("--version", required=True)
    wheel_smoke.set_defaults(function=smoke_wheel_command)

    verify = commands.add_parser("verify-set", help="verify and manifest a release set")
    verify.add_argument("--archives-dir", type=Path, required=True)
    verify.add_argument("--wheels-dir", type=Path, required=True)
    verify.add_argument("--manifests-dir", type=Path, required=True)
    verify.add_argument("--output", type=Path, required=True)
    verify.add_argument("--tag", required=True)
    verify.add_argument("--version", required=True)
    verify.add_argument("--commit", required=True)
    verify.set_defaults(function=verify_release_set)

    checksums = commands.add_parser(
        "prepare-release-assets",
        help="verify the public GitHub Release set and write SHA256SUMS",
    )
    checksums.add_argument("--archives-dir", type=Path, required=True)
    checksums.add_argument("--tag", required=True)
    checksums.set_defaults(function=prepare_release_assets)
    return root


def main() -> int:
    try:
        args = parser().parse_args()
        args.function(args)
    except ArtifactError as error:
        print(f"error: {error}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
