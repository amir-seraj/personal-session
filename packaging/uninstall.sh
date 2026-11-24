#!/usr/bin/env bash
set -euo pipefail
[[ $EUID == 0 ]] || { echo 'Run with sudo.' >&2; exit 1; }
prefix=${DESTDIR:-}/usr/local
dir=$prefix/lib/personal-session/v1.1.0
[[ -f $dir/.personal-session-managed ]] || { echo 'No managed v1.1.0 installation.' >&2; exit 1; }
for c in personal-session personal-start personal-stop code-personal chrome-personal obsidian-personal telegram-personal personal-volume personal-mount personal-umount; do
    file=$prefix/bin/$c
    case "$c" in personal-volume|personal-mount|personal-umount) target=personal-volume;; *) target=personal-session;; esac
    if [[ -L $file && $(readlink "$file") == "../lib/personal-session/v1.1.0/$target" ]]; then rm -- "$file"; fi
done
file=$prefix/share/applications/io.local.PersonalSession.desktop
if [[ -L $file && $(readlink "$file") == ../../lib/personal-session/v1.1.0/io.local.PersonalSession.desktop ]]; then rm -- "$file"; fi
for f in personal-session personal-volume io.local.PersonalSession.desktop LICENSE README.md THIRD_PARTY_NOTICES.txt uninstall.sh .personal-session-managed; do rm -f -- "$dir/$f"; done
rmdir "$dir"
echo 'Uninstalled. Your volume, configuration and sudo policy are retained.'
