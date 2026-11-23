# Unleash

An archive manager for Linux in the spirit of 7-Zip that looks like your Hyprland rice: colours,
corners, shadows and folder icons come from the active hypr-rice, and it changes live when you
switch. Dragging files out of an archive works on Wayland too.

Rust and plain GTK4 (no libadwaita). Made to go with [Burrow](https://github.com/AriCuteGirl/burrow).

## What it does
- Opens zip, 7z, rar, tar, tar.gz/xz/zst/bz2, iso, cab, deb, single .gz/.xz/.zst files and
  everything else 7-Zip reads. The format is found from the file's first bytes, not its name
- Browse folders inside the archive, sort by name, size, packed size or date
- Extract everything or just the selection, with progress and Cancel
- **Drag files out** into file managers, browsers, chat apps and editors (see below for why this
  usually fails on Wayland, and what Unleash does)
- Double-click a file to open it in its app. Save it there and Unleash offers to put the new
  version back into the archive
- Make new archives (New, Ctrl+N, or drop files on an empty window): zip, 7z, tar.gz, tar.xz,
  tar.zst, with a compression level
- Add files (Add, or drop them on the window), delete (Delete) and rename (F2) inside zip, tar,
  tar.gz/xz/zst and 7z archives
- Passwords: opens locked zip, 7z and rar; makes locked zip (AES-256) and 7z (file names hidden
  too). It can remember a password in your keyring
- Test archive (Ctrl+T): reads every file through so the format's checksums run and lists what's
  damaged. It says so when a format can't prove anything (plain tar, zstd without checksums)
- Right-click menu on files; Burrow can hand its Extract here / Compress… to it

## Install
Needs GTK 4.18+. For all formats and remembered passwords (Arch names):

```bash
sudo pacman -S --needed gtk4 7zip libarchive libsecret
```

**Ready-made** (x86_64, glibc 2.39+): download the `.tar.gz` from
[Releases](https://github.com/AriCuteGirl/unleash/releases/latest), unpack it and run
`./install.sh` in its folder.

**Build it yourself** (also needs `git rust base-devel`):

```bash
git clone https://github.com/AriCuteGirl/unleash
cd unleash
./install.sh
```

The installer needs no root: everything goes into `~/.local`. `./install.sh --uninstall` removes
it again. To open archives with it by default:

```bash
xdg-mime default world.puppygirl.Unleash.desktop application/zip application/x-7z-compressed application/vnd.rar application/x-compressed-tar
```

## Hyprland
The app-id is `world.puppygirl.Unleash`. To float the small progress window that `unleash x` /
`unleash a` show (Lua config):

```lua
hl.window_rule({match = {class = "^(world.puppygirl.Unleash)$", title = "^(Extracting|Compressing) .*" }, float = true})
hl.window_rule({match = {class = "^(world.puppygirl.Unleash)$", title = "^(Extracting|Compressing) .*" }, center = true})
```

## Drag and drop on Wayland
Most archive managers drag files out with XDND Direct Save, an X11-only protocol where the drop
target tells the source which folder to extract into. Wayland has nothing like it, so on Hyprland
those drops fail or hand over paths that don't exist yet.

Unleash starts extracting the dragged files the moment the drag starts, into
`~/.cache/unleash/drag/`. It offers them as `text/uri-list`, `application/vnd.portal.filetransfer`
(for Flatpak apps, through the document portal) and plain-text paths. When the target asks for
them, the answer waits until the files are complete. The files stay for a day, because browsers
and chat apps read them while they upload after the drop.

## Command line

```
unleash [ARCHIVE…]                 open archives
unleash x [--to DIR] ARCHIVE…      extract each archive next to it (or into DIR)
unleash a [OPTIONS] FILE…          compress files and folders into one archive
    --format zip|7z|tar.gz|tar.xz|tar.zst   (default zip)
    --name NAME   --to DIR   --level fast|normal|best
    --dialog      ask for name, format and place in the New archive window
```

`x` and `a` show a small progress window and return at once (a running Unleash does the work).
Extracting is smart: one folder inside stays that folder, loose files get a folder named after
the archive. A locked archive asks for its password in that window.

## How each format is read

| Format | How |
| --- | --- |
| zip (stored/deflate, ZipCrypto/AES), tar, tar.gz, tar.zst, tar.xz, .gz/.zst/.xz | built in (Rust crates) |
| tar.bz2, tar.lz4, tar.lzma, tar.Z | `bsdtar` (libarchive) |
| 7z, rar, iso, cab, wim, dmg, zip with other methods, … | `7z` (7zip package) |

## Configuration
`~/.config/unleash/config.toml`, read live:

```toml
[appearance]
colors = "rice"      # follow the active rice, or always one: "gravity-falls" (also in the ☰ menu)
font = "Google Sans Flex"
opacity = 0.94
icon_theme = ""      # "" = the one in ~/.config/gtk-4.0/settings.ini
```

A rice is a folder in `~/.local/share/hypr-rices/<id>/` with a `rice.json` (colours, Noctalia
palette name) and `hypr/look.lua` (rounding, shadow); `~/.local/state/hypr-rices/current` holds
the active id. Without one the built-in Lucid Blocks palette is used, so Unleash works anywhere.

`icon_theme` exists because GTK4 asks the desktop portal for the icon theme, and on some setups
(a KDE portal reading old KDE settings) that's a different theme than the one you chose.

## Safety
Extracting goes into a hidden temp folder first and is moved into place under free names
("notes (2).txt"): nothing is ever overwritten, and a cancelled or failed extract leaves nothing
behind. Paths that try to climb out of the destination (`../`, absolute paths, links pointing
out) are refused. New archives are written under a temp name and renamed when complete. Adding,
deleting and renaming write a new copy of the archive, read it back to check it, and only then
swap it in, so the original stays untouched if anything goes wrong. Passwords go to `7z` and
`secret-tool` through their input, never on a command line, and are only stored in the keyring
when you tick "Remember".

## Development
`cargo test` runs the tests (they make archives with the real `7z` and `bsdtar` when present).
`UNLEASH_APP_ID=world.puppygirl.UnleashTest` starts a second copy that doesn't hand over to the
one already running.

[Changelog](CHANGELOG.md) · [MIT license](LICENSE)
