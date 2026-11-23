#!/bin/sh
# Package a ready-made build: dist/unleash-<version>-<arch>-linux.tar.gz plus its checksum.
set -eu
cd "$(dirname "$0")"
VER=$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)
NAME="unleash-$VER-$(uname -m)-linux"
CARGO="${CARGO_HOME:-$HOME/.cargo}"
# no local paths in the binary
# (separated by \x1f, so folders with spaces work; the last matching one wins)
US=$(printf '\037')
export CARGO_ENCODED_RUSTFLAGS="--remap-path-prefix=$HOME=home$US--remap-path-prefix=$CARGO=cargo$US--remap-path-prefix=$PWD=unleash"
export CARGO_PROFILE_RELEASE_STRIP=true CARGO_TARGET_DIR="$PWD/target/dist"
cargo build --release --locked
if grep -aqF "$HOME" target/dist/release/unleash; then
    echo "The binary still contains $HOME, not packaging it." >&2
    exit 1
fi
rm -rf "dist/$NAME" "dist/$NAME.tar.gz"
mkdir -p "dist/$NAME"
cp target/dist/release/unleash install.sh README.md CHANGELOG.md LICENSE "dist/$NAME/"
cp -r data "dist/$NAME/"
# licences of the libraries built into the binary
OUT="dist/$NAME/THIRD-PARTY-LICENSES.txt"
echo "Unleash includes these Rust libraries, under the licences below." > "$OUT"
cargo metadata --locked --format-version 1 --filter-platform "$(rustc -vV | sed -n 's/^host: //p')" | python3 -c '
import json, os, sys, glob
for p in sorted(json.load(sys.stdin)["packages"], key=lambda p: p["name"]):
    if p["source"] is None:
        continue
    print("\n" + "=" * 72 + "\n{} {}  ({})".format(p["name"], p["version"], p["license"]))
    d = os.path.dirname(p["manifest_path"])
    for f in sorted(glob.glob(d + "/LICEN*") + glob.glob(d + "/COPYRIGHT*") + glob.glob(d + "/COPYING*")):
        if os.path.isfile(f):
            print("\n--- " + os.path.basename(f) + "\n" + open(f, errors="replace").read().strip())
' >> "$OUT"
tar -C dist --owner=0 --group=0 --numeric-owner -czf "dist/$NAME.tar.gz" "$NAME"
cd dist && sha256sum "$NAME.tar.gz" | tee "$NAME.tar.gz.sha256"
