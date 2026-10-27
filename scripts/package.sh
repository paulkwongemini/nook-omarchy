#!/usr/bin/env bash
set -euo pipefail
root=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)
cd -- "$root"
if (( $# > 1 )) || [[ ${1:-} != '' && ${1:-} != --source-only ]]; then
  printf 'Usage: %s [--source-only]\n' "$0" >&2
  exit 2
fi

# Include local uncommitted sources, never a Git snapshot or a whole directory.
files=(
  .gitignore Cargo.toml Cargo.lock LICENSE README.md
  assets/nook-omarchy.svg assets/chat.svg assets/gmail.svg assets/drive.svg assets/calendar.svg
  src/main.rs src/workspace.rs src/policy.rs src/notifications.rs src/settings.rs
  scripts/package.sh scripts/install-local.sh scripts/uninstall-local.sh scripts/licenses.py
  packaging/PKGBUILD packaging/nook-omarchy.desktop
)
for file in "${files[@]}"; do
  if [[ ! -f $file || -L $file ]]; then
    printf 'Missing or symlinked source file: %s\n' "$file" >&2
    exit 1
  fi
done
out=${NOOK_PACKAGE_DIR:-${XDG_CACHE_HOME:-$HOME/.cache}/nook-omarchy-package}
[[ $out == /* ]] || { printf 'Package output path must be absolute.\n' >&2; exit 1; }
if [[ -L $out ]]; then
  printf 'Refusing symlinked package workspace: %s\n' "$out" >&2
  exit 1
fi
out=$(realpath -m -- "$out")
case "$out/" in
  "$root/"*) printf 'Package output must be outside the source tree.\n' >&2; exit 1 ;;
esac
mkdir -p -- "$out"
name=nook-omarchy-0.0.2
archive="$out/$name.tar.gz"
for target in "$archive" "$out/PKGBUILD" "$out/.SRCINFO" "$out/LICENSE"; do
  if [[ -L $target ]]; then
    printf 'Refusing symlinked package output: %s\n' "$target" >&2
    exit 1
  fi
done
tar --sort=name --mtime=@0 --owner=0 --group=0 --numeric-owner \
  --transform="s,^,$name/," -czf "$archive" -- "${files[@]}"
checksum=$(sha256sum -- "$archive")
checksum=${checksum%% *}
sed "s/@SOURCE_SHA256@/$checksum/" packaging/PKGBUILD > "$out/PKGBUILD"
install -m644 -- LICENSE "$out/LICENSE"
(cd -- "$out" && makepkg --printsrcinfo) > "$out/.SRCINFO"
printf 'Source archive: %s\nSHA256: %s\nAUR recipe: %s\nAUR metadata: %s\n' \
  "$archive" "$checksum" "$out/PKGBUILD" "$out/.SRCINFO"
if [[ ${1:-} == --source-only ]]; then
  exit 0
fi
build_root=$(mktemp -d /tmp/nook-omarchy-build.XXXXXX)
trap 'rm -rf -- "$build_root"' EXIT
config="$build_root/makepkg.conf"
{
  printf 'source /etc/makepkg.conf\n'
  printf 'BUILDDIR=%q\nPKGDEST=%q\n' "$build_root" "$out"
  printf "PKGEXT='.pkg.tar.zst'\n"
} > "$config"
mkdir -- "$build_root/start"
cp -- "$out/PKGBUILD" "$archive" "$build_root/start/"
cd -- "$build_root/start"
# Explicit config skips user configuration; unset environment overrides as well.
env -u BUILDDIR -u PKGDEST -u PKGEXT makepkg --config "$config" --cleanbuild --force
env -u BUILDDIR -u PKGDEST -u PKGEXT makepkg --config "$config" --packagelist

