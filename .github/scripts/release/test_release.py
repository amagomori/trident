"""Tests for release.py. Run: python3 -m unittest discover -s .github/scripts/release"""
import hashlib
import os
import plistlib
import re
import shutil
import stat
import subprocess
import tarfile
import tempfile
import unittest
from pathlib import Path

import release

ENV = {"GIT_AUTHOR_NAME": "t", "GIT_AUTHOR_EMAIL": "t@example.com", "GIT_COMMITTER_NAME": "t",
       "GIT_COMMITTER_EMAIL": "t@example.com", "GIT_CONFIG_GLOBAL": os.devnull,
       "GIT_CONFIG_NOSYSTEM": "1"}
HAS_DEB = shutil.which("dpkg-deb") and shutil.which("tic")
SUMS = "aa" * 32 + "  Trident-1.2.3-macos-universal.dmg\n" + "bb" * 32 + \
    "  Trident-1.2.3-linux-x86_64.tar.gz\n"


def png_size(path):
    data = Path(path).read_bytes()
    assert data[:8] == b"\x89PNG\r\n\x1a\n", path
    return int.from_bytes(data[16:20], "big"), int.from_bytes(data[20:24], "big")


class Base(unittest.TestCase):
    def setUp(self):
        self.tmp = Path(tempfile.mkdtemp()).resolve()
        self.addCleanup(shutil.rmtree, self.tmp)
        self.binary = self.tmp / "trident"
        self.binary.write_bytes(b"\x7fELF fake GLIBC_2.17\0GLIBC_2.35\0GLIBC_2.3.4\0")


class Version(Base):
    def test_tag_matches_cargo(self):
        version = release.cargo_version()
        self.assertEqual(release.version_from_tag(f"v{version}"), version)
        self.assertEqual(release.version_from_tag(f"v{version}-rc.1"), f"{version}-rc.1")

    def test_rejects_mismatch_and_bad_tags(self):
        for tag in ("v99.0.0", release.cargo_version(), "v1.2", "latest"):
            with self.assertRaises(release.ReleaseError, msg=tag):
                release.version_from_tag(tag)

    def test_glibc_minimum_is_newest_symbol_version(self):
        self.assertEqual(release.glibc_minimum(self.binary), "2.35")
        self.binary.write_bytes(b"static")
        self.assertIsNone(release.glibc_minimum(self.binary))


class Linux(Base):
    def test_tarball_layout_and_modes(self):
        out = self.tmp / "dist"
        made = release.linux(self.binary, "1.2.3", "x86_64", out) if HAS_DEB else None
        if not HAS_DEB:
            out.mkdir()
            stage = self.tmp / "stage"
            release.stage(stage, self.binary, "")
            release.tarball(stage, "Trident-1.2.3-linux-x86_64", out / "Trident-1.2.3-linux-x86_64.tar.gz")
        path = out / "Trident-1.2.3-linux-x86_64.tar.gz"
        with tarfile.open(path) as tar:
            members = {m.name: m for m in tar.getmembers()}
            top = "Trident-1.2.3-linux-x86_64"
            exe = members[f"{top}/bin/trident"]
            self.assertEqual(exe.mode, 0o755)
            self.assertEqual((exe.uid, exe.gid, exe.mtime), (0, 0, release.epoch()))
            self.assertGreater(exe.mtime, 0)
            for size in release.ICON_SIZES:
                icon = f"{top}/share/icons/hicolor/{size}x{size}/apps/trident.png"
                self.assertEqual(members[icon].mode, 0o644, icon)
            for name in ("share/applications/trident.desktop",
                         "share/trident/terminfo/xterm-trident.terminfo",
                         "share/trident/shell-integration/terminal.bash",
                         "share/trident/shell-integration/terminal.zsh",
                         "share/trident/shell-integration/terminal.fish",
                         "share/doc/trident/LICENSE", "share/doc/trident/OFL.txt"):
                self.assertEqual(members[f"{top}/{name}"].mode, 0o644, name)
            self.assertTrue(all(n == top or n.startswith(top + "/") for n in members))
            desktop = tar.extractfile(f"{top}/share/applications/trident.desktop").read().decode()
        self.assertIn("\nName=Trident\n", desktop)
        self.assertIn("\nExec=trident\n", desktop)
        self.assertIn("\nStartupWMClass=trident\n", desktop)
        first = hashlib.sha256(path.read_bytes()).hexdigest()
        stage = self.tmp / "again"
        release.stage(stage, self.binary, "")
        release.tarball(stage, "Trident-1.2.3-linux-x86_64", self.tmp / "again.tar.gz")
        self.assertEqual(hashlib.sha256((self.tmp / "again.tar.gz").read_bytes()).hexdigest(),
                         first, "tarball is not reproducible")

    @unittest.skipUnless(HAS_DEB, "needs dpkg-deb and tic")
    def test_deb_control_and_contents(self):
        made = release.linux(self.binary, "1.2.3", "x86_64", self.tmp / "dist")
        deb = self.tmp / "dist/Trident-1.2.3-linux-amd64.deb"
        self.assertIn(deb, made)
        field = lambda name: subprocess.run(["dpkg-deb", "-f", str(deb), name], check=True,
                                            capture_output=True, text=True).stdout.strip()
        self.assertEqual(field("Package"), "trident")
        self.assertEqual(field("Version"), "1.2.3")
        self.assertEqual(field("Architecture"), "amd64")
        self.assertTrue(field("Depends").startswith("libc6 (>= 2.35), libgcc-s1"))
        listing = subprocess.run(["dpkg-deb", "-c", str(deb)], check=True, capture_output=True,
                                 text=True).stdout
        self.assertRegex(listing, r"-rwxr-xr-x root/root .* \./usr/bin/trident\n")
        for name in ("./usr/share/terminfo/x/xterm-trident", "./usr/share/doc/trident/copyright",
                     "./usr/share/applications/trident.desktop",
                     "./usr/share/trident/shell-integration/terminal.bash"):
            self.assertIn(name, listing)
        self.assertNotIn("./usr/share/doc/trident/LICENSE", listing)

    @unittest.skipUnless(HAS_DEB, "needs dpkg-deb and tic")
    def test_deb_prerelease_sorts_before_release(self):
        release.linux(self.binary, "1.2.3-rc.1", "x86_64", self.tmp / "dist")
        deb = self.tmp / "dist/Trident-1.2.3-rc.1-linux-amd64.deb"
        version = subprocess.run(["dpkg-deb", "-f", str(deb), "Version"], check=True,
                                 capture_output=True, text=True).stdout.strip()
        self.assertEqual(version, "1.2.3~rc.1")
        subprocess.run(["dpkg", "--compare-versions", version, "lt", "1.2.3"], check=True)

    def test_appdir_layout(self):
        root = self.tmp / "Trident.AppDir"
        release.appdir(self.binary, root)
        self.assertTrue(os.access(root / "AppRun", os.X_OK))
        self.assertTrue(os.access(root / "usr/bin/trident", os.X_OK))
        self.assertEqual(os.readlink(root / ".DirIcon"), "trident.png")
        self.assertTrue((root / "trident.png").is_file())
        self.assertIn("Icon=trident", (root / "trident.desktop").read_text())
        self.assertIn('exec "$here/usr/bin/trident" "$@"', (root / "AppRun").read_text())

    def test_appimagetool_relative_to_cwd(self):
        tool = self.tmp / "appimagetool"
        tool.write_text('#!/bin/sh\nfor a; do last=$a; done\necho "$@" > "$last"\n')
        tool.chmod(0o755)
        (self.tmp / "runtime").write_bytes(b"")
        cwd = os.getcwd()
        os.chdir(self.tmp)
        self.addCleanup(os.chdir, cwd)
        release.appimage(self.binary, "x86_64", self.tmp / "out.AppImage", Path("./appimagetool"),
                         Path("./runtime"))
        args = (self.tmp / "out.AppImage").read_text()
        self.assertIn(f"--runtime-file {self.tmp / 'runtime'} ", args)

    def test_unknown_arch(self):
        with self.assertRaises(release.ReleaseError):
            release.linux(self.binary, "1.2.3", "riscv64", self.tmp / "dist")


class MacOS(Base):
    def test_app_bundle(self):
        made = release.macos(self.binary, "1.2.3-rc.1", self.tmp / "dist", dmg=False)
        app = self.tmp / "dist/Trident.app"
        self.assertEqual(made, [app])
        with open(app / "Contents/Info.plist", "rb") as f:
            info = plistlib.load(f)
        self.assertEqual(info["CFBundleName"], "Trident")
        self.assertEqual(info["CFBundleExecutable"], "trident")
        self.assertEqual(info["CFBundleIdentifier"], "com.amagomori.trident")
        self.assertEqual(info["CFBundlePackageType"], "APPL")
        self.assertEqual(info["CFBundleShortVersionString"], "1.2.3")
        self.assertEqual(info["CFBundleIconFile"], "trident")
        self.assertTrue(os.access(app / "Contents/MacOS/trident", os.X_OK))
        resources = app / "Contents/Resources"
        self.assertEqual((resources / "trident.icns").read_bytes()[:4], b"icns")
        for name in ("shell-integration/terminal.zsh", "terminfo/xterm-trident.terminfo",
                     "LICENSE", "OFL.txt"):
            self.assertTrue((resources / name).is_file(), name)

    @unittest.skipIf(shutil.which("hdiutil"), "macOS builds the .dmg")
    def test_dmg_needs_hdiutil(self):
        with self.assertRaises(release.ReleaseError):
            release.macos(self.binary, "1.2.3", self.tmp / "dist")


class Desktop(unittest.TestCase):
    def test_entry(self):
        text = release.DESKTOP.read_text()
        for line in ("[Desktop Entry]", "Type=Application", "Name=Trident", "Exec=trident",
                     "Icon=trident", "StartupWMClass=trident",
                     "Categories=System;TerminalEmulator;"):
            self.assertIn(line, text.splitlines())
        if shutil.which("desktop-file-validate"):
            subprocess.run(["desktop-file-validate", release.DESKTOP], check=True)

    def test_icons_are_square_at_every_size(self):
        for size in release.ICON_SIZES:
            path = release.ROOT / f"assets/icons/{release.ICON}-{size}.png"
            self.assertEqual(png_size(path), (size, size), path)


class Packages(Base):
    def setUp(self):
        super().setUp()
        (self.tmp / "SHA256SUMS").write_text(SUMS)

    def test_cask(self):
        text = release.cask("1.2.3", "aa" * 32)
        self.assertTrue(text.startswith('cask "trident" do\n  version "1.2.3"\n'))
        self.assertIn(f'  sha256 "{"aa" * 32}"\n', text)
        self.assertIn('  url "https://github.com/amagomori/trident/releases/download/'
                      'v#{version}/Trident-#{version}-macos-universal.dmg"\n', text)
        self.assertIn('  app "Trident.app"\n', text)
        self.assertIn('  binary "#{appdir}/Trident.app/Contents/MacOS/trident"\n', text)
        self.assertIn("strategy :github_latest", text)
        self.assertTrue(text.endswith("end\n"))
        if shutil.which("ruby"):
            path = self.tmp / "trident.rb"
            path.write_text(text)
            subprocess.run(["ruby", "-c", path], check=True, capture_output=True)

    def test_pkgbuild(self):
        text = release.pkgbuild("1.2.3", "bb" * 32)
        lines = text.splitlines()
        for line in ("pkgname=trident-bin", "_tag=1.2.3", "pkgver=1.2.3", "pkgrel=1",
                     "arch=('x86_64')", "provides=('trident')", "conflicts=('trident')",
                     f"sha256sums=('{'bb' * 32}')",
                     'source=("Trident-${_tag}-linux-x86_64.tar.gz::https://github.com/amagomori/'
                     'trident/releases/download/v${_tag}/Trident-${_tag}-linux-x86_64.tar.gz")'):
            self.assertIn(line, lines)
        self.assertIn('  install -Dm755 bin/trident "$pkgdir/usr/bin/trident"', lines)
        if shutil.which("bash"):
            subprocess.run(["bash", "-n"], input=text, text=True, check=True)

    def test_srcinfo_matches_pkgbuild(self):
        text = release.srcinfo("1.2.3", "bb" * 32)
        lines = text.splitlines()
        self.assertEqual(lines[0], "pkgbase = trident-bin")
        self.assertEqual(lines[-1], "pkgname = trident-bin")
        self.assertIn("\tsource = Trident-1.2.3-linux-x86_64.tar.gz::https://github.com/amagomori/"
                      "trident/releases/download/v1.2.3/Trident-1.2.3-linux-x86_64.tar.gz", lines)
        self.assertIn(f"\tsha256sums = {'bb' * 32}", lines)
        self.assertIn("\tpkgver = 1.2.3", lines)
        if shutil.which("makepkg"):
            (self.tmp / "PKGBUILD").write_text(release.pkgbuild("1.2.3", "bb" * 32))
            printed = subprocess.run(["makepkg", "--printsrcinfo"], cwd=self.tmp, check=True,
                                     capture_output=True, text=True).stdout
            self.assertEqual(printed, text)

    def test_aur_version_sorts_prereleases_first(self):
        self.assertEqual(release.aur_version("1.2.3"), "1.2.3")
        self.assertEqual(release.aur_version("1.2.3-rc.1"), "1.2.3rc.1")
        if shutil.which("vercmp"):
            out = subprocess.run(["vercmp", "1.2.3rc.1", "1.2.3"], capture_output=True, text=True)
            self.assertEqual(out.stdout.strip(), "-1")

    def test_packages_uses_release_checksums(self):
        made = release.packages("1.2.3", self.tmp / "SHA256SUMS", self.tmp / "out")
        self.assertEqual([p.relative_to(self.tmp / "out").as_posix() for p in made],
                         ["trident.rb", "aur/PKGBUILD", "aur/.SRCINFO"])
        self.assertIn("aa" * 32, made[0].read_text())
        self.assertIn("bb" * 32, made[1].read_text())
        self.assertIn("bb" * 32, made[2].read_text())

    def test_packages_for_one_platform(self):
        sums = self.tmp / "SHA256SUMS"
        lines = sums.read_text().splitlines()
        for keep, expect in [("macos", ["trident.rb"]), ("linux", ["aur/PKGBUILD", "aur/.SRCINFO"])]:
            sums.write_text("\n".join(l for l in lines if keep in l) + "\n")
            out = self.tmp / f"out-{keep}"
            made = release.packages("1.2.3", sums, out)
            self.assertEqual([p.relative_to(out).as_posix() for p in made], expect)
        sums.write_text("cc  other.zip\n")
        with self.assertRaises(release.ReleaseError):
            release.packages("1.2.3", sums, self.tmp / "out-none")

    def test_base_url_and_missing_asset(self):
        text = release.srcinfo("1.2.3", "bb", base="file:///srv")
        self.assertIn("::file:///srv/Trident-1.2.3-linux-x86_64.tar.gz", text)
        with self.assertRaises(release.ReleaseError):
            release.sha256_of(self.tmp / "SHA256SUMS", "Trident-9.9.9-linux-x86_64.tar.gz")

    def test_is_newest(self):
        tags = ["v0.1.0", "v0.2.0", "v0.3.0-rc.1", "nightly"]
        self.assertTrue(release.is_newest("0.2.0", tags))
        self.assertTrue(release.is_newest("0.2.1", tags))
        self.assertFalse(release.is_newest("0.1.1", tags))
        self.assertTrue(release.is_newest("0.10.0", ["v0.9.0"]))
        self.assertTrue(release.is_newest("1.0.0", []))

    def test_push_package(self):
        script = Path(release.__file__).with_name("push_package.sh")
        remote = self.tmp / "remote.git"
        subprocess.run(["git", "init", "-q", "--bare", "-b", "main", remote], check=True)
        cask = self.tmp / "trident.rb"
        env = {**os.environ, **ENV, "BRANCH": "main"}
        for content, expect in (("one", "pushed"), ("one", "already up to date"),
                                ("two", "pushed")):
            cask.write_text(content)
            out = subprocess.run([script, remote, "Casks", f"trident {content}", cask], env=env,
                                 check=True, capture_output=True, text=True).stdout
            self.assertIn(expect, out)
        show = subprocess.run(["git", "--git-dir", remote, "log", "--format=%s", "main"],
                              capture_output=True, text=True, check=True).stdout
        self.assertEqual(show.split("\n")[:2], ["trident two", "trident one"])
        blob = subprocess.run(["git", "--git-dir", remote, "show", "main:Casks/trident.rb"],
                              capture_output=True, text=True, check=True).stdout
        self.assertEqual(blob, "two")


class Checksums(Base):
    def test_sha256sums(self):
        (self.tmp / "b.deb").write_bytes(b"deb")
        (self.tmp / "SHA256SUMS").write_text("stale")
        lines = release.checksums(self.tmp)
        self.assertEqual([line.split()[1] for line in lines], ["b.deb", "trident"])
        self.assertEqual(lines[0], hashlib.sha256(b"deb").hexdigest() + "  b.deb\n")
        if shutil.which("sha256sum"):
            subprocess.run(["sha256sum", "-c", "--quiet", "SHA256SUMS"], cwd=self.tmp, check=True)


class Changelog(unittest.TestCase):
    def setUp(self):
        self.repo = Path(tempfile.mkdtemp())
        self.addCleanup(shutil.rmtree, self.repo)
        self.git("init", "-q", "-b", "main")
        self.commit("Initial commit")
        self.commit("KAN-1: First feature (#1)")
        self.git("tag", "v0.1.0")
        self.commit("Fix a typo directly on main")
        self.commit("KAN-2: Second feature (#2)")
        self.git("checkout", "-q", "-b", "topic")
        self.commit("work in progress (#99)")
        self.git("checkout", "-q", "main")
        self.git("merge", "-q", "--no-ff", "topic", "-m",
                 "Merge pull request #3 from someone/topic\n\nKAN-3: Merged feature")
        self.git("tag", "v0.2.0")

    def git(self, *args):
        return subprocess.run(["git", *args], cwd=self.repo, check=True, capture_output=True,
                              text=True, env={**os.environ, **ENV}).stdout

    def commit(self, message):
        self.git("commit", "-q", "--allow-empty", "-m", message)

    def test_prs_since_previous_tag(self):
        notes = release.changelog("v0.2.0", cwd=self.repo, repo="o/r")
        self.assertIn("## Trident 0.2.0", notes)
        items = re.findall(r"^- .*$", notes, re.M)
        self.assertEqual(items, ["- KAN-3: Merged feature ([#3](https://github.com/o/r/pull/3))",
                                 "- KAN-2: Second feature ([#2](https://github.com/o/r/pull/2))"])
        self.assertIn("https://github.com/o/r/compare/v0.1.0...v0.2.0", notes)
        self.assertIn("unsigned", notes)
        self.assertIn("xattr -dr com.apple.quarantine /Applications/Trident.app", notes)

    def test_first_release_lists_everything(self):
        notes = release.changelog("v0.1.0", cwd=self.repo, repo="o/r")
        self.assertEqual(re.findall(r"^- .*$", notes, re.M),
                         ["- KAN-1: First feature ([#1](https://github.com/o/r/pull/1))"])
        self.assertNotIn("compare", notes)

    def test_untagged_release_previews_head(self):
        self.commit("KAN-4: Unreleased feature (#4)")
        notes = release.changelog("v0.3.0", cwd=self.repo, repo="o/r")
        self.assertIn("## Trident 0.3.0", notes)
        self.assertEqual(re.findall(r"^- .*$", notes, re.M),
                         ["- KAN-4: Unreleased feature ([#4](https://github.com/o/r/pull/4))"])
        self.assertIn("https://github.com/o/r/compare/v0.2.0...v0.3.0", notes)


class Workflow(unittest.TestCase):
    def test_release_runs_only_on_tags(self):
        text = (release.ROOT / ".github/workflows/release.yml").read_text()
        trigger = re.search(r"^on:\n((?:[ ].*\n|\n)*)", text, re.M).group(1)
        self.assertEqual(re.sub(r"\s+", " ", trigger).strip(), 'push: tags: ["v*"]')

    def test_packages_follow_stable_releases(self):
        text = (release.ROOT / ".github/workflows/release.yml").read_text()
        job = text[text.index("\n  packages:\n"):]
        self.assertIn("needs: publish", job)
        self.assertIn("if: ${{ !contains(github.ref_name, '-') }}", job)
        self.assertIn("if: steps.newest.outputs.newest == 'true' && env.HOMEBREW_TAP_TOKEN != ''", job)
        self.assertIn("if: steps.newest.outputs.newest == 'true' && env.AUR_SSH_KEY != ''", job)
        self.assertNotIn("ssh-keyscan", job)

    def test_linux_files_dated_by_the_tagged_commit(self):
        text = (release.ROOT / ".github/workflows/release.yml").read_text()
        self.assertIn('echo "SOURCE_DATE_EPOCH=$(git log -1 --format=%ct)" >> "$GITHUB_ENV"', text)


if __name__ == "__main__":
    unittest.main()
