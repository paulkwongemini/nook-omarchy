#!/usr/bin/env bash
set -euo pipefail
if (( $# )); then
  printf 'Usage: %s\n' "$0" >&2
  exit 2
fi
prefix=${NOOK_INSTALL_PREFIX:-$HOME/.local}
data=${XDG_DATA_HOME:-$prefix/share}
[[ $prefix == /* && $data == /* ]] || { printf 'Install paths must be absolute.\n' >&2; exit 1; }
manifest="$data/nook-omarchy/install-manifest"
targets=("$prefix/bin/nook-omarchy" "$data/applications/io.github.paulkwongemini.Nook.desktop" "$data/icons/hicolor/scalable/apps/nook-omarchy.svg")
[[ -f $manifest && ! -L $manifest ]] || { printf 'No safe local installation manifest found. Nothing removed.\n' >&2; exit 1; }
mapfile -t previous < "$manifest"
[[ ${#previous[@]} == 3 ]] || { printf 'Invalid install manifest. Nothing removed.\n' >&2; exit 1; }
for digest in "${previous[@]}"; do
  [[ $digest =~ ^[a-f0-9]{64}$ ]] || { printf 'Invalid install manifest. Nothing removed.\n' >&2; exit 1; }
done
status=0
for i in "${!targets[@]}"; do
  target=${targets[i]}
  [[ -e $target || -L $target ]] || continue
  digest=$(sha256sum -- "$target" 2>/dev/null) || digest=''
  if [[ -L $target || ! -f $target || ${digest%% *} != "${previous[i]}" ]]; then
    printf 'Preserved modified or replaced file: %s\n' "$target" >&2
    status=1
    continue
  fi
  rm -- "$target"
done
if (( status == 0 )); then
  rm -- "$manifest"
fi
if command -v update-desktop-database >/dev/null && [[ -d $data/applications ]]; then
  update-desktop-database "$data/applications"
fi
if command -v gtk-update-icon-cache >/dev/null && [[ -d $data/icons/hicolor ]]; then
  gtk-update-icon-cache --force --ignore-theme-index "$data/icons/hicolor" >/dev/null 2>&1 || true
fi
printf 'Local uninstall finished. Login profile and cache were preserved.\n'
exit "$status"

