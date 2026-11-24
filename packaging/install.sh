#!/usr/bin/env bash
set -euo pipefail
cd -- "$(dirname -- "$0")"
[[ $EUID == 0 ]] || { echo 'Run this installer with sudo.' >&2; exit 1; }
prefix=${DESTDIR:-}/usr/local
dir=$prefix/lib/personal-session/v1.1.1
bin=$prefix/bin
desktop=$prefix/share/applications/io.local.PersonalSession.desktop
commands=(personal-session personal-start personal-stop code-personal chrome-personal obsidian-personal telegram-personal personal-volume personal-mount personal-umount)
target() {
    case "$1" in personal-volume|personal-mount|personal-umount) echo personal-volume;; *) echo personal-session;; esac
}
managed_link() {
    local file=$1 relative_base=$2 filename=$3 link
    [[ -L $file ]] || return 1
    link=$(readlink "$file")
    if [[ $link == "$relative_base/v1.1.1/$filename" ]]; then return 0; fi
    local previous
    for previous in v1.1.0 v1.0.0; do
        if [[ -f $prefix/lib/personal-session/$previous/.personal-session-managed &&
              $link == "$relative_base/$previous/$filename" ]]; then return 0; fi
    done
    return 1
}
for c in "${commands[@]}"; do
    file=$bin/$c
    if [[ -e $file || -L $file ]]; then
        managed_link "$file" ../lib/personal-session "$(target "$c")" || { echo "Existing command: $file. Resolve the conflict before installing." >&2; exit 1; }
    fi
done
if [[ -e $desktop || -L $desktop ]]; then
    managed_link "$desktop" ../../lib/personal-session io.local.PersonalSession.desktop || { echo "Existing menu entry: $desktop" >&2; exit 1; }
fi
if [[ -e $dir && ! -f $dir/.personal-session-managed ]]; then echo "Existing install directory: $dir" >&2; exit 1; fi
install -d -m755 "$bin" "$dir" "$(dirname "$desktop")"
touch "$dir/.personal-session-managed"
install -m755 personal-session personal-volume uninstall.sh "$dir/"
install -m644 io.local.PersonalSession.desktop LICENSE README.md THIRD_PARTY_NOTICES.txt "$dir/"
for c in "${commands[@]}"; do ln -sfnT "../lib/personal-session/v1.1.1/$(target "$c")" "$bin/$c"; done
ln -sfnT ../../lib/personal-session/v1.1.1/io.local.PersonalSession.desktop "$desktop"
echo 'Installed Personal Session. Follow the volume setup in README.md before starting.'
