# Changelog

## 0.1.0 (2026-11-23)
First public release: an archive manager for Hyprland that looks like your rice.

- Open, browse and extract zip, 7z, rar, tar.*, iso and everything else 7-Zip reads, with
  progress and Cancel; nothing is ever overwritten
- Drag files out into other apps on Wayland (extracted when the drag starts)
- New archives (zip, 7z, tar.gz/xz/zst); add, delete and rename inside zip, tar.* and 7z, always
  through a checked copy
- Passwords for zip, 7z and rar, optionally remembered in the keyring
- Test archive, right-click menu, edit a file in its app and put it back
- Command line for Burrow: `unleash x`, `unleash a`, `unleash a --dialog`
- Follows the rice live (colours, corners, shadows, folder icons), or keeps one rice's colours
