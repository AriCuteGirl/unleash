#!/bin/sh
# Install Unleash for the current user (no root): the binary next to this script if there is one
# (release download), else a fresh build. ./install.sh --uninstall removes it.
# Your settings in ~/.config/unleash are never touched, and it doesn't become the default by itself.
set -eu
cd "$(dirname "$0")"
BIN="${HOME}/.local/bin"
DATA="${XDG_DATA_HOME:-$HOME/.local/share}"
ID=world.puppygirl.Unleash
if [ "${1:-}" = "--uninstall" ]; then
    rm -f "$BIN/unleash" "$DATA/applications/$ID.desktop" "$DATA/icons/hicolor/scalable/apps/$ID.svg"
    echo "Unleash removed."
    exit 0
fi
if [ -f unleash ]; then
    SRC=unleash
else
    cargo build --release
    SRC=target/release/unleash
fi
install -Dm755 "$SRC" "$BIN/unleash"
install -Dm644 "data/icons/hicolor/scalable/apps/$ID.svg" "$DATA/icons/hicolor/scalable/apps/$ID.svg"
mkdir -p "$DATA/applications"
# xdg-mime and xdg-open don't understand a quoted program, so quote only if the path needs it
case "$BIN" in *[!A-Za-z0-9/._-]*) EXEC="\"$BIN/unleash\"" ;; *) EXEC="$BIN/unleash" ;; esac
sed "s|^Exec=unleash|Exec=$EXEC|" "data/$ID.desktop" > "$DATA/applications/$ID.desktop"
update-desktop-database "$DATA/applications" 2>/dev/null || true
echo "Installed to $BIN/unleash"
echo "Open archives with it by default:  xdg-mime default $ID.desktop application/zip application/x-7z-compressed application/vnd.rar application/x-compressed-tar"
