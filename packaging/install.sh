#!/usr/bin/env bash
set -euo pipefail
cd -- "$(dirname -- "$0")"
[[ $EUID == 0 ]] || { echo 'Run this installer with sudo.' >&2; exit 1; }
prefix=${DESTDIR:-}/usr/local
dir=$prefix/lib/personal-session/v1.0.0
bin=$prefix/bin
desktop=$prefix/share/applications/io.local.PersonalSession.desktop
commands=(personal-session personal-start personal-stop code-personal chrome-personal obsidian-personal telegram-personal personal-volume personal-mount personal-umount)
target() {
    case "$1" in personal-volume|personal-mount|personal-umount) echo personal-volume;; *) echo personal-session;; esac
}
for c in "${commands[@]}"; do
    file=$bin/$c
    if [[ -e $file || -L $file ]]; then
        [[ -L $file && $(readlink "$file") == "../lib/personal-session/v1.0.0/$(target "$c")" ]] || { echo "Existing command: $file. Resolve the conflict before installing." >&2; exit 1; }
    fi
done
if [[ -e $desktop || -L $desktop ]]; then
    [[ -L $desktop && $(readlink "$desktop") == ../../lib/personal-session/v1.0.0/io.local.PersonalSession.desktop ]] || { echo "Existing menu entry: $desktop" >&2; exit 1; }
fi
if [[ -e $dir && ! -f $dir/.personal-session-managed ]]; then echo "Existing install directory: $dir" >&2; exit 1; fi
install -d -m755 "$bin" "$dir" "$(dirname "$desktop")"
touch "$dir/.personal-session-managed"
install -m755 personal-session personal-volume uninstall.sh "$dir/"
install -m644 io.local.PersonalSession.desktop LICENSE README.md THIRD_PARTY_NOTICES.txt "$dir/"
for c in "${commands[@]}"; do ln -sfnT "../lib/personal-session/v1.0.0/$(target "$c")" "$bin/$c"; done
ln -sfnT ../../lib/personal-session/v1.0.0/io.local.PersonalSession.desktop "$desktop"
echo 'Installed Personal Session. Follow the volume setup in README.md before starting.'
