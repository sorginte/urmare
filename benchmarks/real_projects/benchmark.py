#!/usr/bin/env python3
"""Reproducible real-project benchmark orchestration for Urmare.

The public corpus is prepared explicitly with network access. Dry runs,
measurements, smoke tests, unit tests, and summarization are offline.
"""

from __future__ import annotations

import argparse
import dataclasses
import datetime as dt
import hashlib
import json
import math
import os
import platform
import re
import shutil
import stat
import subprocess
import sys
import tempfile
import time
import uuid
from collections import defaultdict
from pathlib import Path, PurePosixPath
from typing import Any, Iterable, Sequence


SCHEMA_VERSION = 1
DEFAULT_SAMPLES = 15
BENCHMARK_EDIT = "# urmare-benchmark: deterministic content-only edit\n"
ROOT_MARKER = ".urmare-real-project-benchmark-root.json"
RUN_RECORDS_PER_SAMPLE = 4
SMOKE_COMMIT = "fb772ab87a18a67302cb644d613512d0fcd39387"
OFFICIAL_REPOSITORIES = {
    "flask": "https://github.com/pallets/flask",
    "fastapi": "https://github.com/fastapi/fastapi",
    "django": "https://github.com/django/django",
    "pandas": "https://github.com/pandas-dev/pandas",
    "airflow": "https://github.com/apache/airflow",
}
SCRIPT_DIRECTORY = Path(__file__).resolve().parent
DEFAULT_MANIFEST = SCRIPT_DIRECTORY / "corpus.json"
REPOSITORY_ROOT = SCRIPT_DIRECTORY.parents[1]


class BenchmarkError(RuntimeError):
    """A reproducibility or safety invariant was violated."""


@dataclasses.dataclass(frozen=True)
class ConfigurationPatch:
    target: str
    mode: str
    patch_file: Path
    base_sha256: str
    result_sha256: str


@dataclasses.dataclass(frozen=True)
class Project:
    id: str
    repository_url: str
    release_reference: str
    git_reference: str
    commit: str
    changed_file: str
    selection_rationale: str
    source_roots: tuple[str, ...]
    test_roots: tuple[str, ...]
    exclusions: tuple[str, ...]
    configuration_patch: ConfigurationPatch | None
    benchmark_command: str
    expected_minimum_python_files: int
    expected_minimum_test_files: int
    special_preparation: bool


@dataclasses.dataclass(frozen=True)
class Corpus:
    path: Path
    selection_policy: str
    projects: tuple[Project, ...]


@dataclasses.dataclass(frozen=True)
class Provenance:
    binary: Path
    binary_sha256: str
    urmare_version: str
    urmare_commit: str
    helper: Path


def portable_path(value: str, field: str, *, python: bool = False) -> str:
    if not value or "\\" in value:
        raise BenchmarkError(f"{field} must be a non-empty portable path")
    path = PurePosixPath(value)
    if path.is_absolute() or ".." in path.parts or "." in path.parts:
        raise BenchmarkError(f"{field} must be normalized and repository-relative: {value!r}")
    if python and path.suffix != ".py":
        raise BenchmarkError(f"{field} must identify a Python file: {value!r}")
    return value


def full_sha(value: str, field: str) -> str:
    if re.fullmatch(r"[0-9a-f]{40}", value) is None:
        raise BenchmarkError(f"{field} must be a full lowercase 40-character Git SHA")
    return value


def sha256_text(value: str, field: str) -> str:
    if re.fullmatch(r"[0-9a-f]{64}", value) is None:
        raise BenchmarkError(f"{field} must be a lowercase SHA-256 digest")
    return value


def load_corpus(path: Path = DEFAULT_MANIFEST, *, official: bool = True) -> Corpus:
    path = path.resolve()
    try:
        document = json.loads(path.read_text(encoding="utf-8"))
    except (OSError, json.JSONDecodeError) as error:
        raise BenchmarkError(f"unable to load corpus manifest {path}: {error}") from error
    if document.get("schema_version") != SCHEMA_VERSION:
        raise BenchmarkError("unsupported corpus manifest schema_version")
    if set(document) != {"schema_version", "selection_policy", "projects"}:
        raise BenchmarkError("corpus manifest contains unknown or missing fields")
    selection_policy = document.get("selection_policy")
    if not isinstance(selection_policy, str) or not selection_policy.strip():
        raise BenchmarkError("corpus selection_policy must be non-empty")
    raw_projects = document.get("projects")
    if not isinstance(raw_projects, list) or not raw_projects:
        raise BenchmarkError("corpus projects must be a non-empty array")

    projects: list[Project] = []
    seen: set[str] = set()
    for raw in raw_projects:
        if not isinstance(raw, dict):
            raise BenchmarkError("every corpus project must be an object")
        expected_fields = {
            "id",
            "repository_url",
            "release_reference",
            "git_reference",
            "commit",
            "changed_file",
            "selection_rationale",
            "source_roots",
            "test_roots",
            "exclusions",
            "configuration_patch",
            "benchmark_command",
            "expected_minimum_python_files",
            "expected_minimum_test_files",
            "special_preparation",
        }
        if set(raw) != expected_fields:
            raise BenchmarkError("corpus project contains unknown or missing fields")
        identifier = raw.get("id")
        if not isinstance(identifier, str) or re.fullmatch(r"[a-z][a-z0-9_-]*", identifier) is None:
            raise BenchmarkError("project id must be a stable lowercase identifier")
        if identifier in seen:
            raise BenchmarkError(f"duplicate project id: {identifier}")
        seen.add(identifier)
        repository_url = raw.get("repository_url")
        if not isinstance(repository_url, str):
            raise BenchmarkError(f"{identifier}: repository_url must be a string")
        if official and OFFICIAL_REPOSITORIES.get(identifier) != repository_url.rstrip("/"):
            raise BenchmarkError(f"{identifier}: repository_url is not the reviewed official URL")
        commit = full_sha(raw.get("commit", ""), f"{identifier}.commit")
        git_reference = raw.get("git_reference")
        if not isinstance(git_reference, str) or not git_reference.startswith("refs/tags/"):
            raise BenchmarkError(f"{identifier}: git_reference must be a full tag ref")
        changed_file = portable_path(
            raw.get("changed_file", ""), f"{identifier}.changed_file", python=True
        )
        release_reference = raw.get("release_reference")
        rationale = raw.get("selection_rationale")
        if not isinstance(release_reference, str) or not release_reference.strip():
            raise BenchmarkError(f"{identifier}: release_reference must be non-empty")
        if not isinstance(rationale, str) or not rationale.strip():
            raise BenchmarkError(f"{identifier}: selection_rationale must be non-empty")

        def string_tuple(field: str, *, paths: bool = False) -> tuple[str, ...]:
            value = raw.get(field)
            if not isinstance(value, list) or not all(isinstance(item, str) for item in value):
                raise BenchmarkError(f"{identifier}.{field} must be an array of strings")
            if paths:
                for item in value:
                    if "*" in item:
                        raise BenchmarkError(f"{identifier}.{field} must list exact roots")
                    if item != ".":
                        portable_path(item, f"{identifier}.{field}")
            if len(value) != len(set(value)):
                raise BenchmarkError(f"{identifier}.{field} contains duplicates")
            return tuple(value)

        source_roots = string_tuple("source_roots", paths=True)
        test_roots = string_tuple("test_roots", paths=True)
        exclusions = string_tuple("exclusions")
        for exclusion in exclusions:
            if (
                not exclusion
                or "\\" in exclusion
                or PurePosixPath(exclusion).is_absolute()
                or ".." in PurePosixPath(exclusion).parts
            ):
                raise BenchmarkError(f"{identifier}.exclusions contains an unsafe pattern")
        patch = parse_configuration_patch(path.parent, identifier, raw.get("configuration_patch"))
        command = raw.get("benchmark_command")
        expected_command = f"urmare --root REPOSITORY impact {changed_file} --json"
        if command != expected_command:
            raise BenchmarkError(
                f"{identifier}: benchmark_command must be exactly {expected_command!r}"
            )
        minimum_python = positive_int(raw.get("expected_minimum_python_files"), identifier)
        minimum_tests = positive_int(raw.get("expected_minimum_test_files"), identifier)
        special = raw.get("special_preparation")
        if not isinstance(special, bool) or special != (patch is not None):
            raise BenchmarkError(
                f"{identifier}: special_preparation must match configuration_patch presence"
            )
        projects.append(
            Project(
                identifier,
                repository_url,
                release_reference,
                git_reference,
                commit,
                changed_file,
                rationale,
                source_roots,
                test_roots,
                exclusions,
                patch,
                command,
                minimum_python,
                minimum_tests,
                special,
            )
        )
    if official and set(seen) != set(OFFICIAL_REPOSITORIES):
        raise BenchmarkError("official corpus must contain exactly the five reviewed projects")
    return Corpus(path, selection_policy, tuple(projects))


def parse_configuration_patch(
    manifest_directory: Path, identifier: str, raw: Any
) -> ConfigurationPatch | None:
    if raw is None:
        return None
    if not isinstance(raw, dict) or raw.get("mode") != "append":
        raise BenchmarkError(f"{identifier}: configuration_patch must use append mode")
    if set(raw) != {
        "target",
        "mode",
        "patch_file",
        "base_sha256",
        "result_sha256",
    }:
        raise BenchmarkError(f"{identifier}: configuration_patch has unknown or missing fields")
    target = portable_path(raw.get("target", ""), f"{identifier}.configuration_patch.target")
    patch_name = portable_path(
        raw.get("patch_file", ""), f"{identifier}.configuration_patch.patch_file"
    )
    patch_file = contained_path(manifest_directory, manifest_directory / patch_name)
    if not patch_file.is_file():
        raise BenchmarkError(f"{identifier}: missing configuration patch {patch_file}")
    return ConfigurationPatch(
        target,
        "append",
        patch_file,
        sha256_text(raw.get("base_sha256", ""), f"{identifier}.base_sha256"),
        sha256_text(raw.get("result_sha256", ""), f"{identifier}.result_sha256"),
    )


def positive_int(value: Any, identifier: str) -> int:
    if not isinstance(value, int) or isinstance(value, bool) or value <= 0:
        raise BenchmarkError(f"{identifier}: expected minimum counts must be positive integers")
    return value


def contained_path(owner: Path, candidate: Path) -> Path:
    owner = owner.resolve()
    candidate = candidate.resolve(strict=False)
    try:
        candidate.relative_to(owner)
    except ValueError as error:
        raise BenchmarkError(f"path escapes benchmark-owned root {owner}: {candidate}") from error
    return candidate


def ensure_owned_root(path: Path, *, create: bool) -> Path:
    path = path.expanduser().resolve(strict=False)
    if path == Path(path.anchor) or path == REPOSITORY_ROOT.resolve():
        raise BenchmarkError(f"refusing broad benchmark working directory: {path}")
    existed = path.exists()
    if create:
        path.mkdir(parents=True, exist_ok=True)
    marker = path / ROOT_MARKER
    if marker.exists():
        try:
            value = json.loads(marker.read_text(encoding="utf-8"))
        except (OSError, json.JSONDecodeError) as error:
            raise BenchmarkError(f"invalid benchmark ownership marker: {marker}") from error
        if value != {"schema_version": 1, "kind": "urmare-real-project-benchmark"}:
            raise BenchmarkError(f"unexpected benchmark ownership marker: {marker}")
    elif create:
        if existed and any(path.iterdir()):
            raise BenchmarkError(
                f"refusing to claim a non-empty directory without an ownership marker: {path}"
            )
        atomic_write_json(marker, {"schema_version": 1, "kind": "urmare-real-project-benchmark"})
    else:
        raise BenchmarkError(f"benchmark working directory was not prepared: {path}")
    return path


def atomic_write_json(path: Path, value: Any) -> None:
    data = (json.dumps(value, sort_keys=True, indent=2) + "\n").encode()
    temporary = path.with_name(f".{path.name}.{uuid.uuid4().hex}.tmp")
    with temporary.open("xb") as stream:
        stream.write(data)
        stream.flush()
        os.fsync(stream.fileno())
    os.replace(temporary, path)


def safe_rmtree(owner: Path, target: Path) -> None:
    owner = owner.resolve()
    target = target.resolve(strict=False)
    try:
        relative = target.relative_to(owner)
    except ValueError as error:
        raise BenchmarkError(f"refusing to delete outside benchmark-owned root: {target}") from error
    if not relative.parts or target == owner:
        raise BenchmarkError(f"refusing broad deletion target: {target}")
    if target.is_symlink():
        raise BenchmarkError(f"refusing to recursively delete a symlink: {target}")
    if target.exists():
        shutil.rmtree(target)


def run_command(
    command: Sequence[str | Path],
    *,
    cwd: Path,
    env: dict[str, str] | None = None,
    check: bool = True,
    text: bool = True,
) -> subprocess.CompletedProcess[Any]:
    completed = subprocess.run(
        [os.fspath(item) for item in command],
        cwd=cwd,
        env=env,
        check=False,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        text=text,
    )
    if check and completed.returncode != 0:
        stderr = completed.stderr if text else completed.stderr.decode(errors="replace")
        raise BenchmarkError(
            f"command failed ({completed.returncode}): {' '.join(map(os.fspath, command))}\n{stderr}"
        )
    return completed


def normalize_repository_url(value: str) -> str:
    value = value.rstrip("/")
    return value[:-4] if value.endswith(".git") else value


def mirror_path(work_root: Path, project: Project) -> Path:
    return work_root / "mirrors" / f"{project.id}.git"


def verify_mirror(work_root: Path, project: Project) -> Path:
    mirror = contained_path(work_root, mirror_path(work_root, project))
    if not mirror.is_dir():
        raise BenchmarkError(f"{project.id}: prepared mirror is missing: {mirror}")
    bare = run_command(["git", "rev-parse", "--is-bare-repository"], cwd=mirror).stdout.strip()
    if bare != "true":
        raise BenchmarkError(f"{project.id}: mirror is not bare")
    remote = run_command(["git", "remote", "get-url", "origin"], cwd=mirror).stdout.strip()
    if normalize_repository_url(remote) != normalize_repository_url(project.repository_url):
        raise BenchmarkError(f"{project.id}: mirror origin does not match manifest")
    revision = run_command(
        ["git", "rev-parse", f"{project.git_reference}^{{commit}}"], cwd=mirror
    ).stdout.strip()
    if revision != project.commit:
        raise BenchmarkError(
            f"{project.id}: {project.git_reference} resolves to {revision}, expected {project.commit}"
        )
    object_type = run_command(
        ["git", "cat-file", "-t", f"{project.commit}:{project.changed_file}"], cwd=mirror
    ).stdout.strip()
    if object_type != "blob":
        raise BenchmarkError(f"{project.id}: changed file is not a blob at pinned commit")
    if project.configuration_patch:
        patch = project.configuration_patch
        upstream = run_command(
            ["git", "show", f"{project.commit}:{patch.target}"], cwd=mirror, text=False
        ).stdout
        if sha256_bytes(upstream) != patch.base_sha256:
            raise BenchmarkError(f"{project.id}: configuration base hash does not match manifest")
        patched = upstream + b"\n" + patch.patch_file.read_bytes()
        if sha256_bytes(patched) != patch.result_sha256:
            raise BenchmarkError(f"{project.id}: configuration result hash does not match manifest")
    return mirror


def prepare_project(work_root: Path, project: Project) -> None:
    mirrors = contained_path(work_root, work_root / "mirrors")
    mirrors.mkdir(exist_ok=True)
    destination = mirror_path(work_root, project)
    if destination.exists():
        verify_mirror(work_root, project)
        print(f"verified {project.id}: {project.commit}")
        return
    staging = mirrors / f".{project.id}.{uuid.uuid4().hex}.preparing"
    try:
        tag = project.git_reference.removeprefix("refs/tags/")
        run_command(
            [
                "git",
                "clone",
                "--bare",
                "--depth",
                "1",
                "--single-branch",
                "--branch",
                tag,
                project.repository_url,
                staging,
            ],
            cwd=mirrors,
        )
        os.replace(staging, destination)
        verify_mirror(work_root, project)
    except Exception:
        safe_rmtree(work_root, staging)
        raise
    print(f"prepared {project.id}: {project.commit}")


def sha256_bytes(value: bytes) -> str:
    return hashlib.sha256(value).hexdigest()


def sha256_file(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as stream:
        for block in iter(lambda: stream.read(1024 * 1024), b""):
            digest.update(block)
    return digest.hexdigest()


def select_projects(corpus: Corpus, selected: Sequence[str]) -> tuple[Project, ...]:
    if not selected or selected == ["all"]:
        return corpus.projects
    if "all" in selected:
        raise BenchmarkError("project 'all' cannot be combined with named projects")
    by_id = {project.id: project for project in corpus.projects}
    unknown = sorted(set(selected) - set(by_id))
    if unknown:
        raise BenchmarkError(f"unknown project(s): {', '.join(unknown)}")
    return tuple(by_id[identifier] for identifier in selected)


def executable(path: Path, field: str) -> Path:
    path = path.expanduser().resolve()
    if not path.is_file() or not os.access(path, os.X_OK):
        raise BenchmarkError(f"{field} is not an executable file: {path}")
    return path


def git_head(path: Path) -> str:
    return full_sha(
        run_command(["git", "rev-parse", "HEAD"], cwd=path).stdout.strip(),
        "Urmare Git commit",
    )


def source_is_clean(path: Path) -> bool:
    return not run_command(
        ["git", "status", "--porcelain", "--untracked-files=all"], cwd=path
    ).stdout


def collect_provenance(binary: Path, helper: Path, *, allow_dirty: bool) -> Provenance:
    binary = executable(binary, "Urmare binary")
    helper = executable(helper, "profiling helper")
    version_output = run_command([binary, "--version"], cwd=REPOSITORY_ROOT).stdout.strip()
    match = re.fullmatch(r"urmare\s+(.+)", version_output)
    if match is None:
        raise BenchmarkError(f"unexpected `urmare --version` output: {version_output!r}")
    commit = git_head(REPOSITORY_ROOT)
    if not allow_dirty and not source_is_clean(REPOSITORY_ROOT):
        raise BenchmarkError(
            "Urmare source tree is dirty; commit benchmark infrastructure and rebuild the binary"
        )
    return Provenance(binary, sha256_file(binary), match.group(1), commit, helper)


def create_checkout(work_root: Path, sample_directory: Path, project: Project) -> Path:
    sample_directory = contained_path(work_root, sample_directory)
    if sample_directory.exists():
        raise BenchmarkError(f"sample directory already exists: {sample_directory}")
    sample_directory.mkdir(parents=True)
    atomic_write_json(
        sample_directory / ".sample-owner.json",
        {"schema_version": 1, "project": project.id, "commit": project.commit},
    )
    repository = sample_directory / "repository"
    mirror = verify_mirror(work_root, project)
    run_command(
        [
            "git",
            "clone",
            "--quiet",
            "--no-checkout",
            "--local",
            "--no-hardlinks",
            mirror,
            repository,
        ],
        cwd=sample_directory,
    )
    run_command(["git", "config", "core.autocrlf", "false"], cwd=repository)
    run_command(["git", "config", "core.eol", "lf"], cwd=repository)
    run_command(["git", "config", "core.filemode", "false"], cwd=repository)
    run_command(
        [
            "git",
            "-c",
            "advice.detachedHead=false",
            "checkout",
            "--quiet",
            "--detach",
            project.commit,
        ],
        cwd=repository,
    )
    verify_checkout(repository, project, expected_changes=set(), configured=False)
    apply_configuration(repository, project)
    commit_configuration(repository, project)
    verify_checkout(repository, project, expected_changes=set(), configured=True)
    return repository


def apply_configuration(repository: Path, project: Project) -> None:
    patch = project.configuration_patch
    if patch is None:
        return
    target = contained_path(repository, repository / patch.target)
    source = target.read_bytes()
    if sha256_bytes(source) != patch.base_sha256:
        raise BenchmarkError(f"{project.id}: checkout configuration base hash is wrong")
    target.write_bytes(source + b"\n" + patch.patch_file.read_bytes())
    if sha256_file(target) != patch.result_sha256:
        raise BenchmarkError(f"{project.id}: applied configuration hash is wrong")


def commit_configuration(repository: Path, project: Project) -> None:
    patch = project.configuration_patch
    if patch is None:
        return
    run_command(["git", "add", "--", patch.target], cwd=repository)
    environment = os.environ.copy()
    environment.update(
        {
            "GIT_AUTHOR_NAME": "Urmare Benchmark",
            "GIT_AUTHOR_EMAIL": "benchmark@example.invalid",
            "GIT_COMMITTER_NAME": "Urmare Benchmark",
            "GIT_COMMITTER_EMAIL": "benchmark@example.invalid",
            "GIT_AUTHOR_DATE": "2025-01-01T00:00:00Z",
            "GIT_COMMITTER_DATE": "2025-01-01T00:00:00Z",
        }
    )
    run_command(
        [
            "git",
            "-c",
            "commit.gpgsign=false",
            "commit",
            "--quiet",
            "--no-gpg-sign",
            "-m",
            "Urmare benchmark configuration",
        ],
        cwd=repository,
        env=environment,
    )


def git_changes(repository: Path) -> set[str]:
    output = run_command(
        ["git", "status", "--porcelain=v1", "--untracked-files=all"], cwd=repository
    ).stdout
    changes: set[str] = set()
    for line in output.splitlines():
        if len(line) < 4:
            raise BenchmarkError(f"malformed git status line: {line!r}")
        status_code, path = line[:2], line[3:]
        if status_code not in {" M", "M "} or " -> " in path:
            raise BenchmarkError(f"unexpected checkout change: {line}")
        portable_path(path, "git status path")
        changes.add(path)
    return changes


def verify_checkout(
    repository: Path,
    project: Project,
    *,
    expected_changes: set[str],
    configured: bool,
) -> None:
    head = run_command(["git", "rev-parse", "HEAD"], cwd=repository).stdout.strip()
    patch = project.configuration_patch
    if configured and patch:
        parent = run_command(["git", "rev-parse", "HEAD^"], cwd=repository).stdout.strip()
        if parent != project.commit:
            raise BenchmarkError(
                f"{project.id}: configured checkout parent is {parent}, expected {project.commit}"
            )
        configured_paths = set(
            run_command(
                ["git", "diff", "--name-only", "HEAD^", "HEAD", "--"], cwd=repository
            ).stdout.splitlines()
        )
        if configured_paths != {patch.target}:
            raise BenchmarkError(f"{project.id}: benchmark configuration commit has extra changes")
    elif head != project.commit:
        raise BenchmarkError(f"{project.id}: checkout is at {head}, expected {project.commit}")
    if git_changes(repository) != expected_changes:
        raise BenchmarkError(
            f"{project.id}: checkout changes do not match deterministic benchmark state"
        )
    changed = contained_path(repository, repository / project.changed_file)
    if not changed.is_file() or changed.is_symlink():
        raise BenchmarkError(f"{project.id}: changed file is missing, not regular, or a symlink")
    if patch:
        expected_hash = patch.result_sha256 if configured else patch.base_sha256
        if sha256_file(repository / patch.target) != expected_hash:
            raise BenchmarkError(f"{project.id}: benchmark configuration state was altered")


def apply_content_only_edit(repository: Path, project: Project) -> None:
    path = contained_path(repository, repository / project.changed_file)
    source = path.read_bytes()
    marker = BENCHMARK_EDIT.encode()
    if marker.rstrip(b"\n") in source:
        raise BenchmarkError(f"{project.id}: deterministic benchmark edit already exists")
    separator = b"" if source.endswith(b"\n") else b"\n"
    path.write_bytes(source + separator + marker)
    verify_checkout(
        repository,
        project,
        expected_changes={project.changed_file},
        configured=True,
    )


def cli_cache_environment(sample_directory: Path) -> tuple[dict[str, str], Path]:
    root = sample_directory / "cache" / "cli-platform"
    if root.exists():
        raise BenchmarkError(f"CLI cache root must not already exist: {root}")
    home = root / "home"
    xdg = root / "xdg-cache"
    local = root / "local-app-data"
    roaming = root / "app-data"
    for directory in (home, xdg, local, roaming):
        directory.mkdir(parents=True)
    environment = os.environ.copy()
    environment.update(
        {
            "HOME": os.fspath(home),
            "USERPROFILE": os.fspath(home),
            "CFFIXED_USER_HOME": os.fspath(home),
            "XDG_CACHE_HOME": os.fspath(xdg),
            "LOCALAPPDATA": os.fspath(local),
            "APPDATA": os.fspath(roaming),
        }
    )
    return environment, root


def assert_cache_contained(sample_directory: Path, cache_root: Path) -> None:
    contained_path(sample_directory, cache_root)
    files = [path for path in cache_root.rglob("*") if path.is_file()]
    if not files:
        raise BenchmarkError("Urmare CLI did not populate the isolated cache")
    for path in files:
        contained_path(cache_root, path)


def exact_cli_command(provenance: Provenance, project: Project) -> list[str]:
    return [
        os.fspath(provenance.binary),
        "--root",
        "repository",
        "impact",
        project.changed_file,
        "--json",
    ]


def profile_command(
    provenance: Provenance,
    project: Project,
    *,
    cache: str | None,
) -> list[str]:
    command = [
        os.fspath(provenance.helper),
        "--root",
        "repository",
        "--changed",
        project.changed_file,
    ]
    if cache is None:
        command.append("--uncached")
    else:
        command.extend(["--cache", cache])
    return command


def parse_json_output(completed: subprocess.CompletedProcess[str], description: str) -> dict[str, Any]:
    if completed.returncode != 0:
        raise BenchmarkError(
            f"{description} failed ({completed.returncode}): {completed.stderr.strip()}"
        )
    if completed.stderr:
        raise BenchmarkError(f"{description} wrote unexpected stderr: {completed.stderr.strip()}")
    try:
        value = json.loads(completed.stdout)
    except json.JSONDecodeError as error:
        raise BenchmarkError(f"{description} emitted malformed JSON: {error}") from error
    if not isinstance(value, dict):
        raise BenchmarkError(f"{description} JSON must be an object")
    return value


def run_profile(
    sample_directory: Path,
    provenance: Provenance,
    project: Project,
    *,
    cache: str | None,
) -> dict[str, Any]:
    completed = run_command(
        profile_command(provenance, project, cache=cache),
        cwd=sample_directory,
        check=False,
    )
    value = parse_json_output(completed, "profiling helper")
    build = value.get("build", {})
    if build.get("profile") != "release":
        raise BenchmarkError("profiling helper must be built in release mode")
    if build.get("git_commit") != provenance.urmare_commit:
        raise BenchmarkError("profiling helper was built from a different Urmare commit")
    if value.get("urmare_version") != provenance.urmare_version:
        raise BenchmarkError("profiling helper and Urmare CLI versions differ")
    if not isinstance(build.get("rustc_version"), str) or build["rustc_version"] == "unknown":
        raise BenchmarkError("profiling helper lacks embedded Rust compiler provenance")
    return value


def run_timed_cli(
    sample_directory: Path,
    environment: dict[str, str],
    provenance: Provenance,
    project: Project,
) -> tuple[subprocess.CompletedProcess[str], int, dict[str, Any]]:
    command = exact_cli_command(provenance, project)
    started = time.perf_counter_ns()
    completed = run_command(command, cwd=sample_directory, env=environment, check=False)
    elapsed = time.perf_counter_ns() - started
    value = parse_json_output(completed, "Urmare CLI")
    validate_impact(value, project)
    return completed, elapsed, value


def validate_impact(value: dict[str, Any], project: Project) -> None:
    if value.get("schema_version") != 1:
        raise BenchmarkError(f"{project.id}: unexpected impact schema version")
    if value.get("changed") != [project.changed_file]:
        raise BenchmarkError(f"{project.id}: impact result changed-file identity is wrong")
    for field in ("directly_affected", "transitively_affected", "affected_tests", "attributions"):
        if not isinstance(value.get(field), list):
            raise BenchmarkError(f"{project.id}: impact result field {field} is malformed")
    for field in ("changed", "directly_affected", "transitively_affected", "affected_tests"):
        for path in value[field]:
            if not isinstance(path, str):
                raise BenchmarkError(f"{project.id}: impact result path is not a string")
            portable_path(path, f"{project.id}.impact.{field}", python=True)
    for attribution in value["attributions"]:
        if not isinstance(attribution, dict) or not isinstance(attribution.get("caused_by"), list):
            raise BenchmarkError(f"{project.id}: impact attribution is malformed")
        affected = attribution.get("affected")
        if not isinstance(affected, str):
            raise BenchmarkError(f"{project.id}: impact attribution path is malformed")
        portable_path(affected, f"{project.id}.impact.attribution", python=True)
        for cause in attribution["caused_by"]:
            if not isinstance(cause, str):
                raise BenchmarkError(f"{project.id}: impact attribution cause is malformed")
            portable_path(cause, f"{project.id}.impact.cause", python=True)


def normalize_result(value: dict[str, Any]) -> dict[str, Any]:
    normalized = json.loads(json.dumps(value))
    if normalized.get("full_validation") is None:
        normalized.pop("full_validation", None)
    return normalized


def result_hash(value: dict[str, Any]) -> str:
    encoded = json.dumps(normalize_result(value), sort_keys=True, separators=(",", ":")).encode()
    return sha256_bytes(encoded)


def validate_profile_result(profile: dict[str, Any], cli_result: dict[str, Any], project: Project) -> None:
    result = profile.get("result")
    if not isinstance(result, dict):
        raise BenchmarkError(f"{project.id}: profiling helper result is malformed")
    validate_impact(normalize_result(result), project)
    if result_hash(result) != result_hash(cli_result):
        raise BenchmarkError(f"{project.id}: CLI and internal-profile impact results differ")


def verify_scale(profile: dict[str, Any], project: Project) -> None:
    repository = profile.get("repository")
    if not isinstance(repository, dict):
        raise BenchmarkError(f"{project.id}: profiling repository counts are malformed")
    python_files = repository.get("python_files")
    tests = repository.get("tests")
    if not isinstance(python_files, int) or python_files < project.expected_minimum_python_files:
        raise BenchmarkError(
            f"{project.id}: discovered {python_files} Python files, expected at least "
            f"{project.expected_minimum_python_files}"
        )
    if not isinstance(tests, int) or tests < project.expected_minimum_test_files:
        raise BenchmarkError(
            f"{project.id}: discovered {tests} test files, expected at least "
            f"{project.expected_minimum_test_files}"
        )


def verify_internal_work(profile: dict[str, Any], scenario: str) -> None:
    work = profile.get("internal_work")
    if not isinstance(work, dict):
        raise BenchmarkError("profiling helper work counters are malformed")
    if scenario == "cold":
        if work.get("build_kind") != "full" or work.get("fallback_reason") != "missing_index":
            raise BenchmarkError("cold profile did not perform a missing-index full build")
    elif scenario == "warm":
        if work.get("build_kind") != "reused":
            raise BenchmarkError("warm profile did not reuse the persistent index")
        bounded = (
            "files_read",
            "files_hashed",
            "files_parsed",
            "importers_reresolved",
            "index_records_written",
            "bytes_written",
        )
        if any(work.get(field) != 0 for field in bounded):
            raise BenchmarkError("warm profile performed unexpected repository work")
    elif scenario == "incremental":
        if work.get("build_kind") != "incremental" or work.get("fallback_reason") is not None:
            raise BenchmarkError("content-only edit did not use a bounded incremental update")
        if any(work.get(field) != 1 for field in ("files_statted", "files_read", "files_hashed", "files_parsed")):
            raise BenchmarkError("incremental update did not inspect exactly the edited file")
        for field in (
            "modules_added",
            "modules_removed",
            "modules_remapped",
            "forward_edges_added",
            "forward_edges_removed",
            "reverse_edges_added",
            "reverse_edges_removed",
        ):
            if work.get(field) != 0:
                raise BenchmarkError(f"content-only edit unexpectedly changed {field}")
        if work.get("importers_reresolved") != 0:
            raise BenchmarkError("content-only edit unexpectedly re-resolved an importer")
        if not isinstance(work.get("index_records_written"), int) or work["index_records_written"] > 2:
            raise BenchmarkError("incremental persistent writes were not bounded")


def machine_metadata() -> dict[str, Any]:
    return {
        "operating_system": platform.system(),
        "os_version": platform.platform(aliased=True, terse=False),
        "architecture": platform.machine(),
        "cpu_description": cpu_description(),
        "logical_core_count": os.cpu_count(),
        "memory_bytes": memory_bytes(),
    }


def cpu_description() -> str | None:
    if sys.platform == "darwin":
        completed = run_command(
            ["sysctl", "-n", "machdep.cpu.brand_string"], cwd=REPOSITORY_ROOT, check=False
        )
        return completed.stdout.strip() or None
    if sys.platform.startswith("linux"):
        try:
            for line in Path("/proc/cpuinfo").read_text(encoding="utf-8").splitlines():
                if line.lower().startswith("model name") and ":" in line:
                    return line.split(":", 1)[1].strip()
        except OSError:
            pass
    return platform.processor() or None


def memory_bytes() -> int | None:
    if sys.platform == "darwin":
        completed = run_command(["sysctl", "-n", "hw.memsize"], cwd=REPOSITORY_ROOT, check=False)
        try:
            return int(completed.stdout.strip())
        except ValueError:
            return None
    if sys.platform.startswith("linux"):
        try:
            for line in Path("/proc/meminfo").read_text(encoding="utf-8").splitlines():
                if line.startswith("MemTotal:"):
                    return int(line.split()[1]) * 1024
        except (OSError, ValueError, IndexError):
            pass
    return None


def utc_timestamp() -> str:
    return dt.datetime.now(dt.timezone.utc).isoformat().replace("+00:00", "Z")


def affected_counts(result: dict[str, Any]) -> dict[str, int]:
    tests = set(result["affected_tests"])
    direct = [path for path in result["directly_affected"] if path not in tests]
    transitive = [path for path in result["transitively_affected"] if path not in tests]
    return {
        "directly_affected_modules": len(direct),
        "transitively_affected_modules": len(transitive),
        "affected_modules": len(set(direct) | set(transitive)),
        "affected_tests": len(tests),
    }


def make_record(
    *,
    run_id: str,
    samples: int,
    project: Project,
    sample_number: int,
    checkout_commit: str,
    scenario: str,
    sample_role: str,
    measured: bool,
    provenance: Provenance,
    machine: dict[str, Any],
    git_version: str,
    completed: subprocess.CompletedProcess[str],
    elapsed_ns: int,
    cli_result: dict[str, Any],
    profile: dict[str, Any],
) -> dict[str, Any]:
    repository_counts = profile["repository"]
    return {
        "schema_version": SCHEMA_VERSION,
        "record_type": "sample",
        "run_id": run_id,
        "run": {"requested_samples": samples},
        "project_id": project.id,
        "upstream_url": (
            project.repository_url if project.id != "smoke" else "local-smoke-fixture"
        ),
        "pinned_upstream_commit": project.commit,
        "benchmark_checkout_commit": checkout_commit,
        "release_reference": project.release_reference,
        "changed_file": project.changed_file,
        "sample_number": sample_number,
        "scenario": scenario,
        "sample_role": sample_role,
        "measured": measured,
        "valid": True,
        "exact_command": exact_cli_command(provenance, project),
        "urmare": {
            "git_commit": provenance.urmare_commit,
            "version": provenance.urmare_version,
            "binary_absolute_path": os.fspath(provenance.binary),
            "binary_sha256": provenance.binary_sha256,
        },
        "timestamp_utc": utc_timestamp(),
        "machine": machine,
        "tools": {
            "rustc_version_used_for_helper": profile["build"]["rustc_version"],
            "git_version": git_version,
        },
        "end_to_end": {
            "elapsed_ns": elapsed_ns,
            "exit_status": completed.returncode,
            "stdout": completed.stdout,
            "stderr": completed.stderr,
        },
        "internal_timings_ns": profile["internal_timings_ns"],
        "internal_work": profile["internal_work"],
        "query_profile": profile["query_profile"],
        "discovered_counts": {
            "python_files": repository_counts["python_files"],
            "test_files": repository_counts["tests"],
            "modules": repository_counts["modules"],
            "import_edges": repository_counts["import_edges"],
            "unresolved_imports": repository_counts["unresolved_imports"],
        },
        "result_counts": affected_counts(cli_result),
        "normalized_result_sha256": result_hash(cli_result),
        "configuration_sha256": (
            project.configuration_patch.result_sha256 if project.configuration_patch else None
        ),
    }


def append_json_line(path: Path, value: dict[str, Any]) -> None:
    payload = (json.dumps(value, sort_keys=True, separators=(",", ":")) + "\n").encode()
    descriptor = os.open(path, os.O_WRONLY | os.O_APPEND)
    try:
        view = memoryview(payload)
        while view:
            written = os.write(descriptor, view)
            if written <= 0:
                raise BenchmarkError(f"unable to append raw result to {path}")
            view = view[written:]
        os.fsync(descriptor)
    finally:
        os.close(descriptor)


def read_json_lines(path: Path) -> list[dict[str, Any]]:
    try:
        data = path.read_bytes()
    except OSError as error:
        raise BenchmarkError(f"unable to read raw results {path}: {error}") from error
    if data and not data.endswith(b"\n"):
        raise BenchmarkError("raw result ends with an interrupted partial JSON line")
    records: list[dict[str, Any]] = []
    for number, line in enumerate(data.splitlines(), start=1):
        try:
            value = json.loads(line)
        except json.JSONDecodeError as error:
            raise BenchmarkError(f"malformed raw result line {number}: {error}") from error
        if not isinstance(value, dict):
            raise BenchmarkError(f"raw result line {number} is not an object")
        records.append(value)
    return records


def run_identifier(
    corpus: Corpus,
    projects: Sequence[Project],
    provenance: Provenance,
    samples: int,
) -> str:
    value = {
        "manifest_sha256": sha256_file(corpus.path),
        "projects": [project.id for project in projects],
        "commits": [project.commit for project in projects],
        "binary_sha256": provenance.binary_sha256,
        "urmare_commit": provenance.urmare_commit,
        "samples": samples,
    }
    digest = sha256_bytes(json.dumps(value, sort_keys=True).encode())
    return digest[:24]


def expected_record_keys() -> set[tuple[str, str]]:
    return {
        ("cold", "measurement"),
        ("warm", "warm_up"),
        ("warm", "measurement"),
        ("incremental", "measurement"),
    }


def completed_samples(
    records: Sequence[dict[str, Any]], run_id: str
) -> set[tuple[str, int]]:
    grouped: dict[tuple[str, int], list[dict[str, Any]]] = defaultdict(list)
    for record in records:
        if record.get("schema_version") != SCHEMA_VERSION or record.get("record_type") != "sample":
            raise BenchmarkError("raw output contains an unsupported record")
        if record.get("run_id") != run_id:
            raise BenchmarkError("raw output belongs to a different benchmark configuration")
        project = record.get("project_id")
        sample = record.get("sample_number")
        if not isinstance(project, str) or not isinstance(sample, int):
            raise BenchmarkError("raw output contains malformed sample identity")
        grouped[(project, sample)].append(record)
    complete: set[tuple[str, int]] = set()
    for identity, group in grouped.items():
        keys = {(item.get("scenario"), item.get("sample_role")) for item in group}
        if len(group) != RUN_RECORDS_PER_SAMPLE or keys != expected_record_keys():
            raise BenchmarkError(
                f"raw output contains ambiguous partial sample {identity[0]} #{identity[1]}"
            )
        if sum(bool(item.get("measured")) for item in group) != 3:
            raise BenchmarkError(f"raw output has invalid warm-up separation for {identity}")
        if any(item.get("valid") is not True for item in group):
            raise BenchmarkError(f"raw output contains invalid records for {identity}")
        if any(item.get("end_to_end", {}).get("exit_status") != 0 for item in group):
            raise BenchmarkError(f"raw output contains a failed invocation for {identity}")
        hashes = {item.get("normalized_result_sha256") for item in group}
        if len(hashes) != 1:
            raise BenchmarkError(f"raw output has inconsistent result hashes for {identity}")
        complete.add(identity)
    return complete


def initialize_output(path: Path, *, resume: bool) -> list[dict[str, Any]]:
    path = path.expanduser().resolve(strict=False)
    path.parent.mkdir(parents=True, exist_ok=True)
    if path.exists():
        if not resume:
            raise BenchmarkError(f"raw output already exists; pass --resume or choose a new path: {path}")
        return read_json_lines(path)
    if resume:
        raise BenchmarkError(f"cannot resume missing raw output: {path}")
    descriptor = os.open(path, os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o644)
    os.close(descriptor)
    return []


def acquire_run_lock(work_root: Path, run_id: str) -> Path:
    directory = work_root / "runs"
    directory.mkdir(exist_ok=True)
    lock = directory / f"{run_id}.lock"
    try:
        descriptor = os.open(lock, os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o644)
    except FileExistsError as error:
        raise BenchmarkError(f"benchmark run lock already exists: {lock}") from error
    with os.fdopen(descriptor, "w", encoding="utf-8") as stream:
        stream.write(f"pid={os.getpid()}\n")
        stream.flush()
        os.fsync(stream.fileno())
    return lock


def quarantine_sample(work_root: Path, sample_directory: Path, run_id: str) -> Path:
    sample_directory = contained_path(work_root, sample_directory)
    if not sample_directory.exists():
        return sample_directory
    destination_directory = work_root / "quarantine" / run_id
    destination_directory.mkdir(parents=True, exist_ok=True)
    destination = destination_directory / f"{sample_directory.name}-{uuid.uuid4().hex}"
    os.replace(sample_directory, destination)
    return destination


def run_one_sample(
    *,
    work_root: Path,
    raw_output: Path,
    run_id: str,
    samples: int,
    project: Project,
    sample_number: int,
    provenance: Provenance,
    machine: dict[str, Any],
    git_version: str,
) -> None:
    sample_directory = work_root / "samples" / run_id / f"{project.id}-{sample_number:03d}"
    repository: Path | None = None
    try:
        repository = create_checkout(work_root, sample_directory, project)
        checkout_commit = git_head(repository)
        validation_profile = run_profile(
            sample_directory, provenance, project, cache=None
        )
        verify_scale(validation_profile, project)
        validate_impact(normalize_result(validation_profile["result"]), project)

        environment, cli_cache = cli_cache_environment(sample_directory)
        profile_cache = sample_directory / "cache" / "profile"
        profile_cache.mkdir()
        if any(profile_cache.iterdir()):
            raise BenchmarkError("internal-profile cache must begin empty")

        completed, elapsed, cli_result = run_timed_cli(
            sample_directory, environment, provenance, project
        )
        cold_profile = run_profile(
            sample_directory, provenance, project, cache="cache/profile"
        )
        validate_profile_result(cold_profile, cli_result, project)
        verify_internal_work(cold_profile, "cold")
        baseline_hash = result_hash(cli_result)
        append_json_line(
            raw_output,
            make_record(
                run_id=run_id,
                samples=samples,
                project=project,
                sample_number=sample_number,
                checkout_commit=checkout_commit,
                scenario="cold",
                sample_role="measurement",
                measured=True,
                provenance=provenance,
                machine=machine,
                git_version=git_version,
                completed=completed,
                elapsed_ns=elapsed,
                cli_result=cli_result,
                profile=cold_profile,
            ),
        )
        assert_cache_contained(sample_directory, cli_cache)

        completed, elapsed, cli_result = run_timed_cli(
            sample_directory, environment, provenance, project
        )
        warmup_profile = run_profile(
            sample_directory, provenance, project, cache="cache/profile"
        )
        validate_profile_result(warmup_profile, cli_result, project)
        verify_internal_work(warmup_profile, "warm")
        if result_hash(cli_result) != baseline_hash:
            raise BenchmarkError(f"{project.id}: warm-up result changed")
        append_json_line(
            raw_output,
            make_record(
                run_id=run_id,
                samples=samples,
                project=project,
                sample_number=sample_number,
                checkout_commit=checkout_commit,
                scenario="warm",
                sample_role="warm_up",
                measured=False,
                provenance=provenance,
                machine=machine,
                git_version=git_version,
                completed=completed,
                elapsed_ns=elapsed,
                cli_result=cli_result,
                profile=warmup_profile,
            ),
        )

        completed, elapsed, cli_result = run_timed_cli(
            sample_directory, environment, provenance, project
        )
        warm_profile = run_profile(
            sample_directory, provenance, project, cache="cache/profile"
        )
        validate_profile_result(warm_profile, cli_result, project)
        verify_internal_work(warm_profile, "warm")
        if result_hash(cli_result) != baseline_hash:
            raise BenchmarkError(f"{project.id}: warm result changed")
        append_json_line(
            raw_output,
            make_record(
                run_id=run_id,
                samples=samples,
                project=project,
                sample_number=sample_number,
                checkout_commit=checkout_commit,
                scenario="warm",
                sample_role="measurement",
                measured=True,
                provenance=provenance,
                machine=machine,
                git_version=git_version,
                completed=completed,
                elapsed_ns=elapsed,
                cli_result=cli_result,
                profile=warm_profile,
            ),
        )

        apply_content_only_edit(repository, project)
        completed, elapsed, cli_result = run_timed_cli(
            sample_directory, environment, provenance, project
        )
        incremental_profile = run_profile(
            sample_directory, provenance, project, cache="cache/profile"
        )
        validate_profile_result(incremental_profile, cli_result, project)
        verify_internal_work(incremental_profile, "incremental")
        if result_hash(cli_result) != baseline_hash:
            raise BenchmarkError(f"{project.id}: content-only edit changed impact output")
        fresh_profile = run_profile(sample_directory, provenance, project, cache=None)
        validate_profile_result(fresh_profile, cli_result, project)
        if result_hash(fresh_profile["result"]) != baseline_hash:
            raise BenchmarkError(f"{project.id}: fresh correctness analysis differs")
        append_json_line(
            raw_output,
            make_record(
                run_id=run_id,
                samples=samples,
                project=project,
                sample_number=sample_number,
                checkout_commit=checkout_commit,
                scenario="incremental",
                sample_role="measurement",
                measured=True,
                provenance=provenance,
                machine=machine,
                git_version=git_version,
                completed=completed,
                elapsed_ns=elapsed,
                cli_result=cli_result,
                profile=incremental_profile,
            ),
        )
        safe_rmtree(work_root, sample_directory)
    except Exception as error:
        quarantine = quarantine_sample(work_root, sample_directory, run_id)
        raise BenchmarkError(
            f"{project.id} sample {sample_number} failed; quarantined at {quarantine}: {error}"
        ) from error


def execute_run(
    *,
    corpus: Corpus,
    projects: Sequence[Project],
    work_root: Path,
    raw_output: Path,
    binary: Path,
    helper: Path,
    samples: int,
    resume: bool,
    allow_dirty: bool,
) -> str:
    if samples <= 0:
        raise BenchmarkError("sample count must be positive")
    raw_output = raw_output.expanduser().resolve(strict=False)
    for transient in (work_root / "samples", work_root / "mirrors", work_root / "quarantine"):
        try:
            raw_output.relative_to(transient.resolve(strict=False))
        except ValueError:
            continue
        raise BenchmarkError(f"raw output cannot be stored below transient benchmark data: {raw_output}")
    for project in projects:
        verify_mirror(work_root, project)
    provenance = collect_provenance(binary, helper, allow_dirty=allow_dirty)
    run_id = run_identifier(corpus, projects, provenance, samples)
    existing = initialize_output(raw_output, resume=resume)
    complete = completed_samples(existing, run_id)
    expected = {
        (project.id, sample_number)
        for project in projects
        for sample_number in range(1, samples + 1)
    }
    if not complete.issubset(expected):
        raise BenchmarkError("raw output contains samples outside the requested project/count set")
    lock = acquire_run_lock(work_root, run_id)
    machine = machine_metadata()
    git_version = run_command(["git", "--version"], cwd=REPOSITORY_ROOT).stdout.strip()
    try:
        for project in projects:
            for sample_number in range(1, samples + 1):
                identity = (project.id, sample_number)
                if identity in complete:
                    stale = work_root / "samples" / run_id / f"{project.id}-{sample_number:03d}"
                    if stale.exists():
                        quarantine = quarantine_sample(work_root, stale, run_id)
                        print(f"resume: quarantined completed sample checkout at {quarantine}")
                    print(f"resume: verified complete {project.id} sample {sample_number}")
                    continue
                print(f"running {project.id} sample {sample_number}/{samples}")
                run_one_sample(
                    work_root=work_root,
                    raw_output=raw_output,
                    run_id=run_id,
                    samples=samples,
                    project=project,
                    sample_number=sample_number,
                    provenance=provenance,
                    machine=machine,
                    git_version=git_version,
                )
    finally:
        lock.unlink(missing_ok=True)
    return run_id


def percentile(values: Sequence[int | float], fraction: float) -> float:
    if not values:
        raise BenchmarkError("cannot summarize an empty observation set")
    ordered = sorted(float(value) for value in values)
    position = (len(ordered) - 1) * fraction
    lower = math.floor(position)
    upper = math.ceil(position)
    if lower == upper:
        return ordered[lower]
    return ordered[lower] + (ordered[upper] - ordered[lower]) * (position - lower)


def statistics(values: Sequence[int | float]) -> dict[str, int | float]:
    if not values:
        raise BenchmarkError("cannot summarize an empty observation set")
    return {
        "sample_count": len(values),
        "median": percentile(values, 0.50),
        "p25": percentile(values, 0.25),
        "p75": percentile(values, 0.75),
        "p95": percentile(values, 0.95),
        "minimum": min(values),
        "maximum": max(values),
    }


def require_number(container: dict[str, Any], field: str, record_number: int) -> int | float:
    value = container.get(field)
    if not isinstance(value, (int, float)) or isinstance(value, bool):
        raise BenchmarkError(f"raw record {record_number} lacks numeric {field}")
    return value


def summarize_records(records: Sequence[dict[str, Any]]) -> dict[str, Any]:
    grouped: dict[tuple[str, str], list[dict[str, Any]]] = defaultdict(list)
    discarded = 0
    run_ids: set[str] = set()
    for number, record in enumerate(records, start=1):
        if record.get("schema_version") != SCHEMA_VERSION or record.get("record_type") != "sample":
            raise BenchmarkError(f"unsupported raw record at line {number}")
        run_id = record.get("run_id")
        if not isinstance(run_id, str):
            raise BenchmarkError(f"raw record {number} lacks run_id")
        run_ids.add(run_id)
        if not record.get("valid"):
            continue
        if not record.get("measured"):
            discarded += 1
            continue
        project = record.get("project_id")
        scenario = record.get("scenario")
        if not isinstance(project, str) or scenario not in {"cold", "warm", "incremental"}:
            raise BenchmarkError(f"raw record {number} has invalid grouping fields")
        grouped[(project, scenario)].append(record)
    if not grouped:
        raise BenchmarkError("raw results contain no measured observations")

    groups: list[dict[str, Any]] = []
    for (project, scenario), observations in sorted(grouped.items()):
        end_to_end: list[int | float] = []
        index_total: list[int | float] = []
        impact_query: list[int | float] = []
        query_total: list[int | float] = []
        counters: dict[str, list[int | float]] = defaultdict(list)
        hashes: set[str] = set()
        for observation in observations:
            number = records.index(observation) + 1
            end_to_end.append(
                require_number(observation.get("end_to_end", {}), "elapsed_ns", number)
            )
            index_total.append(
                require_number(observation.get("internal_timings_ns", {}), "index_total", number)
            )
            query = observation.get("query_profile", {})
            impact_query.append(require_number(query, "impact_query", number))
            query_total.append(require_number(query, "query_total", number))
            work = observation.get("internal_work")
            if not isinstance(work, dict):
                raise BenchmarkError(f"raw record {number} lacks internal_work")
            for key, value in work.items():
                if isinstance(value, (int, float)) and not isinstance(value, bool):
                    counters[key].append(value)
            result_digest = observation.get("normalized_result_sha256")
            if not isinstance(result_digest, str):
                raise BenchmarkError(f"raw record {number} lacks normalized result hash")
            hashes.add(result_digest)
        groups.append(
            {
                "project_id": project,
                "scenario": scenario,
                "latency_ns": {
                    "end_to_end_cli": statistics(end_to_end),
                    "internal_index_update": statistics(index_total),
                    "impact_query": statistics(impact_query),
                    "internal_query_total": statistics(query_total),
                },
                "work_counters": {
                    key: statistics(values) for key, values in sorted(counters.items())
                },
                "normalized_result_hashes": sorted(hashes),
            }
        )
    return {
        "schema_version": SCHEMA_VERSION,
        "source_run_ids": sorted(run_ids),
        "discarded_warm_up_records": discarded,
        "groups": groups,
    }


def milliseconds(value: int | float) -> str:
    return f"{float(value) / 1_000_000:.3f}"


def summary_markdown(summary: dict[str, Any]) -> str:
    lines = [
        "# Proposed real-project benchmark summary",
        "",
        "> Generated from preserved raw JSON Lines for human review. This file does not update project documentation automatically.",
        "",
        f"Discarded warm-up records: {summary['discarded_warm_up_records']}",
        "",
        "## Latency",
        "",
        "All latency values are milliseconds. End-to-end CLI time is not derived from internal phase totals.",
        "",
        "| Project | Scenario | Layer | n | median | p25 | p75 | p95 | min | max |",
        "|---|---|---|---:|---:|---:|---:|---:|---:|---:|",
    ]
    layer_names = {
        "end_to_end_cli": "end-to-end CLI",
        "internal_index_update": "internal index/update",
        "impact_query": "impact query",
        "internal_query_total": "internal query total",
    }
    for group in summary["groups"]:
        for key, label in layer_names.items():
            stats = group["latency_ns"][key]
            lines.append(
                "| {project} | {scenario} | {layer} | {sample_count} | {median} | {p25} | "
                "{p75} | {p95} | {minimum} | {maximum} |".format(
                    project=group["project_id"],
                    scenario=group["scenario"],
                    layer=label,
                    sample_count=stats["sample_count"],
                    median=milliseconds(stats["median"]),
                    p25=milliseconds(stats["p25"]),
                    p75=milliseconds(stats["p75"]),
                    p95=milliseconds(stats["p95"]),
                    minimum=milliseconds(stats["minimum"]),
                    maximum=milliseconds(stats["maximum"]),
                )
            )
    lines.extend(
        [
            "",
            "## Work counters",
            "",
            "Counters are summarized independently from latency.",
            "",
            "| Project | Scenario | Counter | n | median | p25 | p75 | p95 | min | max |",
            "|---|---|---|---:|---:|---:|---:|---:|---:|---:|",
        ]
    )
    for group in summary["groups"]:
        for counter, stats in group["work_counters"].items():
            lines.append(
                "| {project} | {scenario} | {counter} | {sample_count} | {median:g} | "
                "{p25:g} | {p75:g} | {p95:g} | {minimum:g} | {maximum:g} |".format(
                    project=group["project_id"],
                    scenario=group["scenario"],
                    counter=counter,
                    **stats,
                )
            )
    lines.append("")
    return "\n".join(lines)


def write_text_exclusive(path: Path, value: str) -> None:
    path = path.expanduser().resolve(strict=False)
    path.parent.mkdir(parents=True, exist_ok=True)
    try:
        with path.open("x", encoding="utf-8", newline="\n") as stream:
            stream.write(value)
            stream.flush()
            os.fsync(stream.fileno())
    except FileExistsError as error:
        raise BenchmarkError(f"refusing to overwrite summary output: {path}") from error


def create_smoke_project(work_root: Path) -> Project:
    fixture = work_root / "smoke-fixture-v1"
    source = fixture / "source"
    patch_file = fixture / "config" / "urmare.toml"
    patch_file.parent.mkdir(parents=True, exist_ok=True)
    patch_content = "[tool.urmare]\nsource-roots = [\"src\"]\n"
    if patch_file.exists() and patch_file.read_text(encoding="utf-8") != patch_content:
        raise BenchmarkError("local smoke configuration patch was altered")
    patch_file.write_text(patch_content, encoding="utf-8", newline="\n")
    if not source.exists():
        source.mkdir(parents=True)
        files = {
            "pyproject.toml": "[project]\nname = \"urmare-benchmark-smoke\"\n",
            "src/demo/__init__.py": "\"\"\"Smoke package.\"\"\"\n",
            "src/demo/core.py": "VALUE = 1\n",
            "src/demo/service.py": "from demo import core\n\ndef value():\n    return core.VALUE\n",
            "tests/test_service.py": "from demo import service\n\ndef test_value():\n    assert service.value() == 1\n",
        }
        for relative, content in files.items():
            destination = source / relative
            destination.parent.mkdir(parents=True, exist_ok=True)
            destination.write_text(content, encoding="utf-8", newline="\n")
        run_command(["git", "init", "--quiet", "--initial-branch=main"], cwd=source)
        run_command(["git", "config", "core.autocrlf", "false"], cwd=source)
        run_command(["git", "config", "core.filemode", "false"], cwd=source)
        run_command(["git", "add", "."], cwd=source)
        environment = os.environ.copy()
        environment.update(
            {
                "GIT_AUTHOR_NAME": "Urmare Benchmark",
                "GIT_AUTHOR_EMAIL": "benchmark@example.invalid",
                "GIT_COMMITTER_NAME": "Urmare Benchmark",
                "GIT_COMMITTER_EMAIL": "benchmark@example.invalid",
                "GIT_AUTHOR_DATE": "2025-01-01T00:00:00Z",
                "GIT_COMMITTER_DATE": "2025-01-01T00:00:00Z",
            }
        )
        run_command(
            [
                "git",
                "-c",
                "commit.gpgsign=false",
                "commit",
                "--quiet",
                "--no-gpg-sign",
                "--no-verify",
                "-m",
                "smoke fixture",
            ],
            cwd=source,
            env=environment,
        )
    if not source_is_clean(source):
        raise BenchmarkError("local smoke source is dirty")
    commit = git_head(source)
    if commit != SMOKE_COMMIT:
        raise BenchmarkError(
            f"local smoke fixture commit is {commit}, expected deterministic {SMOKE_COMMIT}"
        )
    base_configuration = (source / "pyproject.toml").read_bytes()
    patched_configuration = base_configuration + b"\n" + patch_file.read_bytes()
    project = Project(
        "smoke",
        os.fspath(source.resolve()),
        "local deterministic smoke fixture",
        "refs/heads/main",
        commit,
        "src/demo/core.py",
        "Small local fixture with one production importer and one affected test.",
        ("src",),
        (),
        (),
        ConfigurationPatch(
            "pyproject.toml",
            "append",
            patch_file,
            sha256_bytes(base_configuration),
            sha256_bytes(patched_configuration),
        ),
        "urmare --root REPOSITORY impact src/demo/core.py --json",
        4,
        1,
        True,
    )
    mirrors = work_root / "mirrors"
    mirrors.mkdir(exist_ok=True)
    mirror = mirror_path(work_root, project)
    if not mirror.exists():
        staging = mirrors / f".smoke.{uuid.uuid4().hex}.preparing"
        try:
            run_command(["git", "clone", "--quiet", "--bare", source, staging], cwd=mirrors)
            os.replace(staging, mirror)
        except Exception:
            safe_rmtree(work_root, staging)
            raise
    verify_mirror(work_root, project)
    return project


def dry_run_report(
    *,
    corpus: Corpus,
    projects: Sequence[Project],
    work_root: Path,
    binary: Path,
    helper: Path,
    allow_dirty: bool,
) -> dict[str, Any]:
    provenance = collect_provenance(binary, helper, allow_dirty=allow_dirty)
    resolved = []
    for project in projects:
        mirror = verify_mirror(work_root, project)
        resolved.append(
            {
                "project_id": project.id,
                "upstream_url": (
                    project.repository_url if project.id != "smoke" else "local-smoke-fixture"
                ),
                "release_reference": project.release_reference,
                "pinned_commit": project.commit,
                "prepared_mirror": os.fspath(mirror),
                "changed_file": project.changed_file,
                "source_roots": project.source_roots,
                "test_roots": project.test_roots,
                "exclusions": project.exclusions,
                "configuration": (
                    {
                        "target": project.configuration_patch.target,
                        "patch_file": os.fspath(project.configuration_patch.patch_file),
                        "result_sha256": project.configuration_patch.result_sha256,
                    }
                    if project.configuration_patch
                    else None
                ),
                "command": exact_cli_command(provenance, project),
            }
        )
    return {
        "schema_version": SCHEMA_VERSION,
        "mode": "dry_run",
        "selection_policy": corpus.selection_policy,
        "binary_sha256": provenance.binary_sha256,
        "urmare_git_commit": provenance.urmare_commit,
        "projects": resolved,
    }


def add_project_argument(parser: argparse.ArgumentParser) -> None:
    parser.add_argument(
        "--project",
        action="append",
        default=[],
        metavar="ID",
        help="Select one project; repeat for multiple projects (default: all).",
    )


def add_runtime_arguments(parser: argparse.ArgumentParser) -> None:
    parser.add_argument("--work-dir", type=Path, required=True)
    parser.add_argument("--binary", type=Path, required=True)
    parser.add_argument("--profile-helper", type=Path, required=True)


def argument_parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--manifest", type=Path, default=DEFAULT_MANIFEST)
    commands = parser.add_subparsers(dest="command", required=True)

    prepare = commands.add_parser("prepare", help="Clone and verify pinned public repositories.")
    prepare.add_argument("--work-dir", type=Path, required=True)
    add_project_argument(prepare)

    dry_run = commands.add_parser("dry-run", help="Resolve and print a prepared run without measuring.")
    add_runtime_arguments(dry_run)
    add_project_argument(dry_run)
    dry_run.add_argument(
        "--allow-dirty-source",
        action="store_true",
        help="Allow development-only dry-run validation before infrastructure is committed.",
    )

    run = commands.add_parser("run", help="Run paired cold, warm, and incremental samples.")
    add_runtime_arguments(run)
    add_project_argument(run)
    run.add_argument("--samples", type=int, default=DEFAULT_SAMPLES)
    run.add_argument("--output", type=Path, required=True)
    run.add_argument("--resume", action="store_true")

    smoke = commands.add_parser("smoke", help="Run the lifecycle against an offline local fixture.")
    add_runtime_arguments(smoke)
    smoke.add_argument("--output", type=Path)
    smoke.add_argument("--dry-run", action="store_true")

    summarize = commands.add_parser("summarize", help="Summarize preserved raw JSON Lines.")
    summarize.add_argument("--input", type=Path, required=True)
    summarize.add_argument("--output", type=Path, required=True)
    summarize.add_argument("--format", choices=("markdown", "json"), default="markdown")
    return parser


def main(arguments: Sequence[str] | None = None) -> int:
    options = argument_parser().parse_args(arguments)
    try:
        if options.command == "summarize":
            summary = summarize_records(read_json_lines(options.input.expanduser().resolve()))
            output = (
                summary_markdown(summary)
                if options.format == "markdown"
                else json.dumps(summary, sort_keys=True, indent=2) + "\n"
            )
            write_text_exclusive(options.output, output)
            print(f"wrote proposed summary: {options.output.expanduser().resolve()}")
            return 0

        corpus = load_corpus(options.manifest)
        if options.command == "prepare":
            work_root = ensure_owned_root(options.work_dir, create=True)
            projects = select_projects(corpus, options.project)
            for project in projects:
                prepare_project(work_root, project)
            return 0

        if options.command == "smoke":
            work_root = ensure_owned_root(options.work_dir, create=True)
            project = create_smoke_project(work_root)
            smoke_corpus = Corpus(corpus.path, "Offline deterministic local smoke fixture.", (project,))
            if options.dry_run:
                print(
                    json.dumps(
                        dry_run_report(
                            corpus=smoke_corpus,
                            projects=(project,),
                            work_root=work_root,
                            binary=options.binary,
                            helper=options.profile_helper,
                            allow_dirty=True,
                        ),
                        sort_keys=True,
                        indent=2,
                    )
                )
                return 0
            if options.output is None:
                raise BenchmarkError("smoke requires --output unless --dry-run is used")
            run_id = execute_run(
                corpus=smoke_corpus,
                projects=(project,),
                work_root=work_root,
                raw_output=options.output.expanduser().resolve(strict=False),
                binary=options.binary,
                helper=options.profile_helper,
                samples=1,
                resume=False,
                allow_dirty=True,
            )
            print(f"completed offline smoke run {run_id}")
            return 0

        work_root = ensure_owned_root(options.work_dir, create=False)
        projects = select_projects(corpus, options.project)
        if options.command == "dry-run":
            print(
                json.dumps(
                    dry_run_report(
                        corpus=corpus,
                        projects=projects,
                        work_root=work_root,
                        binary=options.binary,
                        helper=options.profile_helper,
                        allow_dirty=options.allow_dirty_source,
                    ),
                    sort_keys=True,
                    indent=2,
                )
            )
            return 0
        run_id = execute_run(
            corpus=corpus,
            projects=projects,
            work_root=work_root,
            raw_output=options.output.expanduser().resolve(strict=False),
            binary=options.binary,
            helper=options.profile_helper,
            samples=options.samples,
            resume=options.resume,
            allow_dirty=False,
        )
        print(f"completed benchmark run {run_id}")
        return 0
    except BenchmarkError as error:
        print(f"error: {error}", file=sys.stderr)
        return 2


if __name__ == "__main__":
    raise SystemExit(main())
