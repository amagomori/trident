#!/usr/bin/env bash
# Stage the matching non-V8 PDFium build and its redistribution notices.
set -euo pipefail
platform=${1:?usage: pdfium.sh linux-x64|linux-arm64|mac-universal OUT}
out=${2:?missing output directory}
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
fetch() {
  local target=$1 sha=$2 archive="$work/$1.tgz"
  curl -fsSL --retry 3 "https://github.com/bblanchon/pdfium-binaries/releases/download/chromium/8125/pdfium-$target.tgz" -o "$archive"
  printf '%s  %s\n' "$sha" "$archive" | shasum -a 256 -c -
  mkdir "$work/$target"
  tar -xzf "$archive" -C "$work/$target" lib LICENSE licenses
}
case "$platform" in
  linux-x64) fetch "$platform" e1c3ce6e270cb5e0c20816b379ad9f2c1309972697c505f508de92bf5e7d6659 ;;
  linux-arm64) fetch "$platform" 8b8818f2ec57d1b213d952ca6d7f0ffeca9d0110487d0e20ad83ba918bc92efa ;;
  mac-universal)
    fetch mac-arm64 4939bd935ac83ddc09a3a4f038bc43a19cea1660316a3667f26cb07a070e139d
    fetch mac-x64 8a5193535b80c7215d8932f6647ae5097465b6dba2811602c21e120c2af112b0
    platform=mac-arm64
    diff -r "$work/mac-arm64/licenses" "$work/mac-x64/licenses"
    lipo -create "$work"/mac-*/lib/libpdfium.dylib -output "$work/universal.dylib"
    lipo "$work/universal.dylib" -verify_arch arm64 x86_64
    mv "$work/universal.dylib" "$work/$platform/lib/libpdfium.dylib"
    ;;
  *) echo "unsupported PDFium platform: $platform" >&2; exit 1 ;;
esac
mkdir -p "$out"
cp -R "$work/$platform"/{lib,LICENSE,licenses} "$out/"
