#!/usr/bin/env bash
set -euo pipefail
root=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)
if (( $# > 1 )); then
  printf 'Usage: %s [build-directory]\n' "$0" >&2
  exit 2
fi
build=${1:-${CARGO_TARGET_DIR:-${XDG_CACHE_HOME:-$HOME/.cache}/nook-omarchy-build}/release}
prefix=${NOOK_INSTALL_PREFIX:-$HOME/.local}
data=${XDG_DATA_HOME:-$prefix/share}
[[ $prefix == /* && $data == /* ]] || { printf 'Install paths must be absolute.\n' >&2; exit 1; }
manifest="$data/nook-omarchy/install-manifest"
targets=("$prefix/bin/nook-omarchy" "$data/applications/io.github.paulkwongemini.Nook.desktop" "$data/icons/hicolor/scalable/apps/nook-omarchy.svg")
[[ -f $build/nook-omarchy && -x $build/nook-omarchy ]] || { printf 'Build the application first: %s/nook-omarchy\n' "$build" >&2; exit 1; }
previous=()
if [[ -e $manifest || -L $manifest ]]; then
  [[ -f $manifest && ! -L $manifest ]] || { printf 'Unsafe install manifest.\n' >&2; exit 1; }
  mapfile -t previous < "$manifest"
  [[ ${#previous[@]} == 3 ]] || { printf 'Invalid install manifest.\n' >&2; exit 1; }
  for digest in "${previous[@]}"; do
    [[ $digest =~ ^[a-f0-9]{64}$ ]] || { printf 'Invalid install manifest.\n' >&2; exit 1; }
  done
fi
for i in "${!targets[@]}"; do
  target=${targets[i]}
  if [[ -e $target || -L $target ]]; then
    digest=$(sha256sum -- "$target" 2>/dev/null) || digest=''
    if [[ -L $target || ! -f $target || ${#previous[@]} != 3 || ${digest%% *} != "${previous[i]:-}" ]]; then
      printf 'Refusing to overwrite an unmanaged or modified file: %s\n' "$target" >&2
      exit 1
    fi
  fi
done

stage=$(mktemp -d)
trap 'rm -rf -- "$stage"' EXIT
# Quote the absolute executable for the desktop entry, including literal %.
executable=${targets[0]//\\/\\\\}
executable=${executable//\"/\\\"}
executable=${executable//\`/\\\`}
executable=${executable//\$/\\\$}
executable=${executable//%/%%}
while IFS= read -r line; do
  if [[ $line == Exec=* ]]; then
    printf 'Exec="%s"\n' "$executable"
  else
    printf '%s\n' "$line"
  fi
done < "$root/packaging/nook-omarchy.desktop" > "$stage/nook.desktop"
install -Dm755 -- "$build/nook-omarchy" "${targets[0]}"
install -Dm644 -- "$stage/nook.desktop" "${targets[1]}"
install -Dm644 -- "$root/assets/nook-omarchy.svg" "${targets[2]}"
mkdir -p -- "$(dirname -- "$manifest")"
for target in "${targets[@]}"; do
  digest=$(sha256sum -- "$target")
  printf '%s\n' "${digest%% *}"
done > "$stage/manifest"
install -m600 -- "$stage/manifest" "$manifest"
if command -v update-desktop-database >/dev/null; then
  update-desktop-database "$data/applications"
fi
if command -v gtk-update-icon-cache >/dev/null; then
  gtk-update-icon-cache --force --ignore-theme-index "$data/icons/hicolor" >/dev/null 2>&1 || true
fi
printf 'Installed Nook to %s\nLaunch from your application menu or run: %s\n' "$prefix" "${targets[0]}"

