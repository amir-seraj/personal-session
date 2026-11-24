#!/usr/bin/env bash
set -euo pipefail
cd -- "$(dirname -- "$0")/.."
[[ $(uname -sm) == 'Linux x86_64' ]] || { echo 'This release target requires x86_64 Linux.' >&2; exit 1; }
# Keep developer home and dependency cache paths out of the distributed ELF files.
export RUSTFLAGS="--remap-path-prefix=$HOME=/build"
export CFLAGS="-ffile-prefix-map=$HOME=/build" CXXFLAGS="-ffile-prefix-map=$HOME=/build"
cargo test --release --locked -j2
cargo build --release --locked -j2
name=personal-session-1.0.0-x86_64-linux-gnu
mkdir -p dist
stage=$(mktemp -d)
trap 'rm -rf -- "$stage"' EXIT
mkdir "$stage/$name"
install -m755 target/release/{personal-session,personal-volume} packaging/{install,uninstall}.sh "$stage/$name"
install -m644 README.md LICENSE io.local.PersonalSession.desktop "$stage/$name"
cp -r examples "$stage/$name"
cp -r assets "$stage/$name"
python3 scripts/notices.py > "$stage/$name/THIRD_PARTY_NOTICES.txt"
tar -C "$stage" --sort=name --owner=0 --group=0 --numeric-owner -czf "dist/$name.tar.gz" "$name"
(cd dist && sha256sum "$name.tar.gz" > SHA256SUMS)
