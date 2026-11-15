#!/usr/bin/env python3
"""Packages Trident release artifacts from built binaries (KAN-45).

Builds are unsigned: the macOS app is only ad-hoc signed (`codesign -s -`), which Apple
Silicon needs to run it at all; there is no Developer ID signature or notarization.

  release.py version --tag v0.1.0                   check the tag against Cargo.toml
  release.py linux --binary B --version V --out D    tarball, .deb and (with --appimagetool
                    [--appimagetool T --runtime R]   and --runtime) an AppImage
  release.py macos --binary B --version V --out D    Trident.app and, where hdiutil exists, a .dmg
  release.py checksums D                            SHA256SUMS for every file in D
  release.py changelog --tag T [--since PREV]       release notes from the PRs merged since PREV

Stdlib only; needs dpkg-deb (and tic, for the compiled terminfo entry) for the .deb. Tests:
python3 -m unittest discover -s .github/scripts/release
"""
import argparse
import gzip
import hashlib
import io
import os
import plistlib
import re
import shutil
import stat
import subprocess
import sys
import tarfile
import tempfile
from pathlib import Path

ROOT = Path(__file__).resolve().parents[3]
NAME = "Trident"
PACKAGE = "trident"
BUNDLE_ID = "com.amagomori.trident"
REPO = "amagomori/terminal"
HOMEPAGE = f"https://github.com/{REPO}"
SUMMARY = "Fast, GPU-rendered terminal emulator"
MACOS_MINIMUM = "11.0"
DEB_ARCH = {"x86_64": "amd64", "aarch64": "arm64"}
# The binary links only libc, libm and libgcc_s; these are dlopened at runtime.
DEB_DEPENDS = ["libgcc-s1", "libfontconfig1", "libxkbcommon0"]
DEB_RECOMMENDS = ["libxkbcommon-x11-0", "libx11-xcb1", "libxcursor1", "libxi6",
                  "libwayland-client0", "libwayland-egl1", "libegl1", "libvulkan1"]

DESKTOP = f"""[Desktop Entry]
Type=Application
Name={NAME}
GenericName=Terminal
Comment={SUMMARY}
Exec={PACKAGE}
Icon={PACKAGE}
Terminal=false
Categories=System;TerminalEmulator;
Keywords=shell;prompt;command;commandline;terminal;
StartupWMClass={PACKAGE}
"""

APPRUN = f"""#!/bin/sh
here=$(dirname "$(readlink -f "$0")")
exec "$here/usr/bin/{PACKAGE}" "$@"
"""


class ReleaseError(Exception):
    pass


def cargo_version(root=ROOT):
    text = (root / "Cargo.toml").read_text()
    section = re.search(r"^\[workspace\.package\]\n(.*?)(?=^\[|\Z)", text, re.M | re.S)
    match = section and re.search(r'^version\s*=\s*"([^"]+)"', section.group(1), re.M)
    if not match:
        raise ReleaseError("no [workspace.package] version in Cargo.toml")
    return match.group(1)


def version_from_tag(tag, root=ROOT):
    if not re.fullmatch(r"v\d+\.\d+\.\d+(-[0-9A-Za-z.-]+)?", tag):
        raise ReleaseError(f"tag {tag!r} is not vMAJOR.MINOR.PATCH[-PRERELEASE]")
    version = tag[1:]
    if version.split("-")[0] != cargo_version(root):
        raise ReleaseError(f"tag {tag} does not match Cargo.toml version {cargo_version(root)}")
    return version


def glibc_minimum(binary):
    found = re.findall(rb"GLIBC_(\d+)\.(\d+)(?:\.(\d+))?", Path(binary).read_bytes())
    if not found:
        return None
    best = max(tuple(int(p or 0) for p in v) for v in found)
    return ".".join(str(p) for p in (best if best[2] else best[:2]))


def share_files(prefix):
    """(source, destination under prefix, mode) for everything installed beside the binary."""
    out = [(ROOT / "assets/icons/tide-256.png",
            f"{prefix}share/icons/hicolor/256x256/apps/{PACKAGE}.png", 0o644),
           (None, f"{prefix}share/applications/{PACKAGE}.desktop", 0o644),
           (ROOT / "crates/term-app/terminfo/xterm-trident.terminfo",
            f"{prefix}share/{PACKAGE}/terminfo/xterm-trident.terminfo", 0o644),
           (ROOT / "LICENSE", f"{prefix}share/doc/{PACKAGE}/LICENSE", 0o644),
           (ROOT / "crates/term-render/assets/fonts/OFL.txt",
            f"{prefix}share/doc/{PACKAGE}/OFL.txt", 0o644),
           (ROOT / "README.md", f"{prefix}share/doc/{PACKAGE}/README.md", 0o644)]
    for script in sorted((ROOT / "shell-integration").iterdir()):
        out.append((script, f"{prefix}share/{PACKAGE}/shell-integration/{script.name}", 0o644))
    return out


def stage(root, binary, prefix):
    """Lays out bin/ and share/ under root/prefix."""
    root = Path(root)
    files = [(Path(binary), f"{prefix}bin/{PACKAGE}", 0o755)] + share_files(prefix)
    for source, dest, mode in files:
        target = root / dest
        target.parent.mkdir(parents=True, exist_ok=True)
        if source is None:
            target.write_text(DESKTOP)
        else:
            shutil.copyfile(source, target)
        target.chmod(mode)


def epoch():
    """The timestamp packaged files carry: SOURCE_DATE_EPOCH, else the commit time.

    Never 0: KDE's menu cache skips .desktop files dated 1970-01-01.
    """
    if "SOURCE_DATE_EPOCH" in os.environ:
        return max(1, int(os.environ["SOURCE_DATE_EPOCH"]))
    commit = subprocess.run(["git", "log", "-1", "--format=%ct"], cwd=Path(__file__).parent,
                            capture_output=True, text=True)
    return int(commit.stdout.strip() or 0) or 1


def tarball(src_dir, top, out):
    """A reproducible .tar.gz of src_dir's contents under top/."""
    def normalize(info):
        info.uid = info.gid = 0
        info.uname = info.gname = ""
        info.mtime = epoch()
        if info.isdir() or info.mode & 0o111:
            info.mode = 0o755
        else:
            info.mode = 0o644
        return info

    buf = io.BytesIO()
    with tarfile.open(fileobj=buf, mode="w", format=tarfile.GNU_FORMAT) as tar:
        tar.add(src_dir, arcname=top, recursive=False, filter=normalize)
        for path in sorted(Path(src_dir).rglob("*")):
            tar.add(path, arcname=f"{top}/{path.relative_to(src_dir)}", recursive=False,
                    filter=normalize)
    with open(out, "wb") as raw, gzip.GzipFile(fileobj=raw, mode="wb", mtime=epoch(),
                                               filename="") as gz:
        gz.write(buf.getvalue())


def deb(binary, version, arch, out):
    if not shutil.which("dpkg-deb"):
        raise ReleaseError("dpkg-deb not found")
    if not shutil.which("tic"):
        raise ReleaseError("tic not found (ncurses-bin)")
    with tempfile.TemporaryDirectory() as tmp:
        root = Path(tmp) / "root"
        stage(root, binary, "usr/")
        info = root / "usr/share/terminfo"
        info.mkdir(parents=True)
        subprocess.run(["tic", "-x", "-o", str(info),
                        str(ROOT / "crates/term-app/terminfo/xterm-trident.terminfo")],
                       check=True, capture_output=True)
        doc = root / f"usr/share/doc/{PACKAGE}"
        (doc / "copyright").write_text(
            f"Format: https://www.debian.org/doc/packaging-manuals/copyright-format/1.0/\n"
            f"Upstream-Name: {NAME}\nSource: {HOMEPAGE}\n\n"
            f"Files: *\nLicense: MIT\n\n"
            f"Files: usr/bin/{PACKAGE}\nComment: embeds the JetBrains Mono fonts\n"
            f"License: OFL-1.1\n\nSee LICENSE and OFL.txt in this directory.\n")
        (doc / "LICENSE").unlink()
        size = sum(p.stat().st_size for p in root.rglob("*") if p.is_file())
        depends = list(DEB_DEPENDS)
        glibc = glibc_minimum(binary)
        depends.insert(0, f"libc6 (>= {glibc})" if glibc else "libc6")
        (root / "DEBIAN").mkdir()
        (root / "DEBIAN/control").write_text(
            f"Package: {PACKAGE}\nVersion: {version.replace('-', '~', 1)}\nArchitecture: {DEB_ARCH[arch]}\n"
            f"Maintainer: {NAME} developers <noreply@github.com>\n"
            f"Installed-Size: {(size + 1023) // 1024}\n"
            f"Depends: {', '.join(depends)}\nRecommends: {', '.join(DEB_RECOMMENDS)}\n"
            f"Section: x11\nPriority: optional\nHomepage: {HOMEPAGE}\n"
            f"Description: {SUMMARY}\n"
            f" {NAME} is a fast, GPU-rendered terminal emulator for Linux and macOS,\n"
            f" written from scratch in Rust.\n")
        for path in root.rglob("*"):
            if path.is_dir():
                path.chmod(0o755)
        env = dict(os.environ, SOURCE_DATE_EPOCH=str(epoch()))
        subprocess.run(["dpkg-deb", "--root-owner-group", "-Zxz", "--build", str(root), str(out)],
                       check=True, capture_output=True, env=env)


def appdir(binary, path):
    path = Path(path)
    stage(path, binary, "usr/")
    (path / f"{PACKAGE}.desktop").write_text(DESKTOP)
    shutil.copyfile(ROOT / "assets/icons/tide-256.png", path / f"{PACKAGE}.png")
    (path / ".DirIcon").symlink_to(f"{PACKAGE}.png")
    (path / "AppRun").write_text(APPRUN)
    (path / "AppRun").chmod(0o755)


def appimage(binary, arch, out, tool, runtime):
    with tempfile.TemporaryDirectory() as tmp:
        root = Path(tmp) / f"{NAME}.AppDir"
        appdir(binary, root)
        env = dict(os.environ, ARCH=arch, APPIMAGE_EXTRACT_AND_RUN="1",
                   SOURCE_DATE_EPOCH=str(epoch()))
        result = subprocess.run([str(Path(tool).resolve()), "--no-appstream", "--runtime-file",
                                 str(Path(runtime).resolve()),
                                 str(root), str(out)], capture_output=True, text=True, env=env)
        if result.returncode != 0:
            raise ReleaseError(f"appimagetool failed:\n{result.stdout}{result.stderr}")


def linux(binary, version, arch, out, tool=None, runtime=None):
    if arch not in DEB_ARCH:
        raise ReleaseError(f"unsupported arch {arch}")
    out = Path(out)
    out.mkdir(parents=True, exist_ok=True)
    top = f"{NAME}-{version}-linux-{arch}"
    made = []
    with tempfile.TemporaryDirectory() as tmp:
        stage(tmp, binary, "")
        made.append(out / f"{top}.tar.gz")
        tarball(tmp, top, made[-1])
    made.append(out / f"{NAME}-{version}-linux-{DEB_ARCH[arch]}.deb")
    deb(binary, version, arch, made[-1])
    if tool:
        made.append(out / f"{NAME}-{version}-{arch}.AppImage")
        appimage(binary, arch, made[-1], tool, runtime)
    return made


def info_plist(version):
    return {
        "CFBundleDevelopmentRegion": "en",
        "CFBundleDisplayName": NAME,
        "CFBundleExecutable": PACKAGE,
        "CFBundleIconFile": PACKAGE,
        "CFBundleIdentifier": BUNDLE_ID,
        "CFBundleInfoDictionaryVersion": "6.0",
        "CFBundleName": NAME,
        "CFBundlePackageType": "APPL",
        "CFBundleShortVersionString": version.split("-")[0],
        "CFBundleVersion": version.split("-")[0],
        "LSApplicationCategoryType": "public.app-category.developer-tools",
        "LSMinimumSystemVersion": MACOS_MINIMUM,
        "NSHighResolutionCapable": True,
        "NSHumanReadableCopyright": "MIT License",
        "NSSupportsAutomaticGraphicsSwitching": True,
    }


def app_bundle(binary, version, path):
    contents = Path(path) / "Contents"
    (contents / "MacOS").mkdir(parents=True)
    resources = contents / "Resources"
    resources.mkdir()
    with open(contents / "Info.plist", "wb") as f:
        plistlib.dump(info_plist(version), f)
    exe = contents / "MacOS" / PACKAGE
    shutil.copyfile(binary, exe)
    exe.chmod(0o755)
    shutil.copyfile(ROOT / "assets/icons/tide.icns", resources / f"{PACKAGE}.icns")
    for source, dest, mode in share_files(""):
        if source is not None and dest.startswith(f"share/{PACKAGE}/"):
            target = resources / dest.removeprefix(f"share/{PACKAGE}/")
            target.parent.mkdir(parents=True, exist_ok=True)
            shutil.copyfile(source, target)
            target.chmod(mode)
    for doc in ("LICENSE", "crates/term-render/assets/fonts/OFL.txt"):
        shutil.copyfile(ROOT / doc, resources / Path(doc).name)
    if shutil.which("codesign"):
        subprocess.run(["codesign", "--force", "--deep", "--sign", "-", str(path)], check=True,
                       capture_output=True)


def macos(binary, version, out, dmg=True):
    out = Path(out)
    out.mkdir(parents=True, exist_ok=True)
    app = out / f"{NAME}.app"
    if app.exists():
        shutil.rmtree(app)
    app_bundle(binary, version, app)
    if not dmg:
        return [app]
    if not shutil.which("hdiutil"):
        raise ReleaseError("hdiutil not found (the .dmg needs macOS); pass --no-dmg")
    image = out / f"{NAME}-{version}-macos-universal.dmg"
    with tempfile.TemporaryDirectory() as tmp:
        shutil.copytree(app, Path(tmp) / app.name, symlinks=True)
        (Path(tmp) / "Applications").symlink_to("/Applications")
        subprocess.run(["hdiutil", "create", "-volname", NAME, "-srcfolder", tmp, "-fs", "HFS+",
                        "-format", "UDZO", "-ov", str(image)], check=True, capture_output=True)
    return [app, image]


def checksums(directory):
    directory = Path(directory)
    lines = []
    for path in sorted(p for p in directory.iterdir() if p.is_file() and p.name != "SHA256SUMS"):
        digest = hashlib.sha256(path.read_bytes()).hexdigest()
        lines.append(f"{digest}  {path.name}\n")
    (directory / "SHA256SUMS").write_text("".join(lines))
    return lines


def git(*args, cwd=ROOT):
    return subprocess.run(["git", *args], cwd=cwd, check=True, capture_output=True,
                          text=True).stdout


def previous_tag(tag, cwd=ROOT):
    result = subprocess.run(["git", "describe", "--tags", "--abbrev=0", "--match", "v*",
                             f"{tag}^"], cwd=cwd, capture_output=True, text=True)
    return result.stdout.strip() if result.returncode == 0 else None


def merged_prs(rev, cwd=ROOT):
    """(number, title) for each PR merged in rev on the first-parent line, newest first."""
    out = []
    for record in git("log", "--first-parent", "--format=%s%x1f%b%x1e", rev, cwd=cwd).split("\x1e"):
        if not record.strip():
            continue
        subject, _, body = record.strip("\n").partition("\x1f")
        merge = re.fullmatch(r"Merge pull request #(\d+) from \S+", subject)
        if merge:
            title = next((line.strip() for line in body.splitlines() if line.strip()), subject)
            out.append((int(merge.group(1)), title))
            continue
        squash = re.fullmatch(r"(.*\S) \(#(\d+)\)", subject)
        if squash:
            out.append((int(squash.group(2)), squash.group(1)))
    return out


def changelog(tag, since=None, cwd=ROOT, repo=REPO):
    since = since or previous_tag(tag, cwd)
    prs = merged_prs(f"{since}..{tag}" if since else tag, cwd)
    lines = [f"## {NAME} {tag.removeprefix('v')}", ""]
    if prs:
        lines += ["### Merged pull requests", ""]
        lines += [f"- {title} ([#{n}](https://github.com/{repo}/pull/{n}))" for n, title in prs]
    else:
        lines.append("No pull requests merged since the last release.")
    lines += ["", "Builds are unsigned. On macOS, open Trident.app from the .dmg with "
              "right-click > Open the first time (or run `xattr -dr com.apple.quarantine "
              "/Applications/Trident.app`). Verify downloads with `sha256sum -c SHA256SUMS`."]
    if since:
        lines += ["", f"**Full changelog**: https://github.com/{repo}/compare/{since}...{tag}"]
    return "\n".join(lines) + "\n"


def main(argv=None):
    ap = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    sub = ap.add_subparsers(dest="command", required=True)
    p = sub.add_parser("version")
    p.add_argument("--tag", required=True)
    for name in ("linux", "macos"):
        p = sub.add_parser(name)
        p.add_argument("--binary", required=True, type=Path)
        p.add_argument("--version", required=True)
        p.add_argument("--out", required=True, type=Path)
    p = sub.choices["linux"]
    p.add_argument("--arch", default="x86_64", choices=sorted(DEB_ARCH))
    p.add_argument("--appimagetool", type=Path)
    p.add_argument("--runtime", type=Path, help="AppImage type2 runtime for --appimagetool")
    sub.choices["macos"].add_argument("--no-dmg", action="store_true")
    p = sub.add_parser("checksums")
    p.add_argument("dir", type=Path)
    p = sub.add_parser("changelog")
    p.add_argument("--tag", required=True)
    p.add_argument("--since", help="previous tag (default: the last v* tag before --tag)")
    args = ap.parse_args(argv)
    try:
        if args.command == "version":
            print(version_from_tag(args.tag))
        elif args.command == "linux":
            if bool(args.appimagetool) != bool(args.runtime):
                raise ReleaseError("--appimagetool and --runtime go together")
            for path in linux(args.binary, args.version, args.arch, args.out, args.appimagetool,
                              args.runtime):
                print(path)
        elif args.command == "macos":
            for path in macos(args.binary, args.version, args.out, not args.no_dmg):
                print(path)
        elif args.command == "checksums":
            sys.stdout.write("".join(checksums(args.dir)))
        else:
            sys.stdout.write(changelog(args.tag, args.since))
    except (ReleaseError, subprocess.CalledProcessError) as error:
        detail = getattr(error, "stderr", None)
        print(f"release.py: {error}{': ' + detail.strip() if detail else ''}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
