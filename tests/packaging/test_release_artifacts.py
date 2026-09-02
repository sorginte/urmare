import json
import sys
import tempfile
import unittest
import zipfile
from argparse import Namespace
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parents[2] / "scripts"))

import release_artifacts as release  # noqa: E402


class ReleaseArtifactTests(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory()
        self.root = Path(self.temporary.name)
        self.repository = self.root / "repository"
        self.repository.mkdir()
        (self.repository / "Cargo.toml").write_text(
            '[workspace.package]\nversion = "0.1.0"\n', encoding="utf-8"
        )
        (self.repository / "README.md").write_text("# Urmare\n", encoding="utf-8")
        (self.repository / "LICENSE").write_text("MIT\n", encoding="utf-8")
        self.commit = "a" * 40

    def tearDown(self):
        self.temporary.cleanup()

    def wheel(self, directory: Path, target: str, binary=b"release-binary") -> Path:
        directory.mkdir(parents=True, exist_ok=True)
        platform = {
            "aarch64-apple-darwin": "macosx_11_0_arm64",
            "x86_64-apple-darwin": "macosx_10_12_x86_64",
            "aarch64-unknown-linux-gnu": "manylinux_2_17_aarch64.manylinux2014_aarch64",
            "x86_64-unknown-linux-gnu": "manylinux_2_17_x86_64.manylinux2014_x86_64",
            "x86_64-pc-windows-msvc": "win_amd64",
        }[target]
        wheel = directory / f"urmare-0.1.0-py3-none-{platform}.whl"
        dist_info = "urmare-0.1.0.dist-info"
        data = "urmare-0.1.0.data/scripts"
        executable = "urmare.exe" if target.endswith("windows-msvc") else "urmare"
        expanded_tags = "\n".join(
            f"Tag: py3-none-{platform_tag}" for platform_tag in platform.split(".")
        )
        with zipfile.ZipFile(wheel, "w") as archive:
            archive.writestr(
                f"{dist_info}/METADATA",
                "Metadata-Version: 2.4\nName: urmare\nVersion: 0.1.0\nRequires-Python: >=3.9\n\n",
            )
            archive.writestr(
                f"{dist_info}/WHEEL",
                f"Wheel-Version: 1.0\n{expanded_tags}\n\n",
            )
            archive.writestr(f"{data}/{executable}", binary)
        return wheel

    def package(self, target: str):
        wheel_dir = self.root / "wheels" / target
        archive_dir = self.root / "archives" / target
        manifest_dir = self.root / "manifests" / target
        wheel = self.wheel(wheel_dir, target)
        release.package_target(
            Namespace(
                repository=self.repository,
                wheel_dir=wheel_dir,
                archive_dir=archive_dir,
                manifest_dir=manifest_dir,
                target=target,
                tag="v0.1.0",
                version="0.1.0",
                commit=self.commit,
            )
        )
        return wheel, next(archive_dir.iterdir()), next(manifest_dir.iterdir())

    def test_workspace_version_is_the_single_release_version_source(self):
        self.assertEqual(release.workspace_version(self.repository / "Cargo.toml"), "0.1.0")
        release.validate_release_identity("v0.1.0", "0.1.0", self.commit)
        with self.assertRaises(release.ArtifactError):
            release.validate_release_identity("v0.1.1", "0.1.0", self.commit)

    def test_packages_the_exact_wheel_binary_in_the_standalone_archive(self):
        wheel, archive, manifest_path = self.package("aarch64-apple-darwin")
        _, wheel_binary = release.validate_wheel(wheel, "aarch64-apple-darwin", "0.1.0")
        archive_binary = release.archive_binary(
            archive,
            "urmare-v0.1.0-aarch64-apple-darwin",
            "urmare",
        )
        self.assertEqual(wheel_binary, archive_binary)
        manifest = json.loads(manifest_path.read_text(encoding="utf-8"))
        self.assertEqual(manifest["binary_sha256"], release.sha256_bytes(wheel_binary))

    def test_expands_compressed_wheel_filename_tags(self):
        self.assertEqual(
            release.expanded_compatibility_tags(
                "py3",
                "none",
                "manylinux_2_17_x86_64.manylinux2014_x86_64",
            ),
            [
                "py3-none-manylinux2014_x86_64",
                "py3-none-manylinux_2_17_x86_64",
            ],
        )

    def test_verifies_exact_five_target_channels_and_writes_audit_manifest(self):
        archives = self.root / "combined-archives"
        wheels = self.root / "combined-wheels"
        manifests = self.root / "combined-manifests"
        archives.mkdir()
        wheels.mkdir()
        manifests.mkdir()
        for target in release.TARGETS:
            wheel, archive, manifest = self.package(target)
            wheel.replace(wheels / wheel.name)
            archive.replace(archives / archive.name)
            manifest.replace(manifests / manifest.name)
        output = self.root / "audit" / "release-manifest.json"
        release.verify_release_set(
            Namespace(
                archives_dir=archives,
                wheels_dir=wheels,
                manifests_dir=manifests,
                output=output,
                tag="v0.1.0",
                version="0.1.0",
                commit=self.commit,
            )
        )
        combined = json.loads(output.read_text(encoding="utf-8"))
        self.assertEqual(combined["commit_sha"], self.commit)
        self.assertEqual(len(combined["artifacts"]), 5)

    def test_rejects_an_unexpected_distribution_file(self):
        archives = self.root / "archives"
        wheels = self.root / "wheels"
        manifests = self.root / "manifests"
        archives.mkdir()
        wheels.mkdir()
        manifests.mkdir()
        (wheels / "urmare-0.1.0.tar.gz").write_bytes(b"sdist")
        with self.assertRaisesRegex(release.ArtifactError, "exactly five .whl"):
            release.verify_release_set(
                Namespace(
                    archives_dir=archives,
                    wheels_dir=wheels,
                    manifests_dir=manifests,
                    output=self.root / "manifest.json",
                    tag="v0.1.0",
                    version="0.1.0",
                    commit=self.commit,
                )
            )

    def test_github_release_assets_exclude_wheels_and_include_checksums(self):
        archives = self.root / "github-release"
        archives.mkdir()
        for target in release.TARGETS:
            _, archive, _ = self.package(target)
            archive.replace(archives / archive.name)

        release.prepare_release_assets(
            Namespace(archives_dir=archives, tag="v0.1.0")
        )
        expected = {
            release.expected_archive_name("v0.1.0", target)
            for target in release.TARGETS
        }
        self.assertEqual(
            {path.name for path in archives.iterdir()}, expected | {"SHA256SUMS"}
        )
        lines = (archives / "SHA256SUMS").read_text(encoding="utf-8").splitlines()
        self.assertEqual(len(lines), 5)
        self.assertTrue(all(not line.endswith(".whl") for line in lines))

    def test_github_release_assets_reject_a_wheel(self):
        archives = self.root / "github-release"
        archives.mkdir()
        for target in release.TARGETS:
            _, archive, _ = self.package(target)
            archive.replace(archives / archive.name)
        (archives / "urmare-0.1.0-py3-none-any.whl").write_bytes(b"wheel")

        with self.assertRaisesRegex(release.ArtifactError, "exactly the five"):
            release.prepare_release_assets(
                Namespace(archives_dir=archives, tag="v0.1.0")
            )


if __name__ == "__main__":
    unittest.main()
