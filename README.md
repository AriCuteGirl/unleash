# Unleash

An archive manager for Linux in the spirit of 7-Zip that looks like your Hyprland rice.
Rust + GTK4, no libadwaita.

- Open zip, 7z, rar, tar, tar.gz/xz/zst/bz2, iso, cab, deb, single .gz/.xz/.zst files and
  everything else 7-Zip reads
- Browse folders inside the archive, sort by name, size, packed size or date
- Extract everything or just the selection, with progress and Cancel. Nothing is ever
  overwritten: results get free names ("notes (2).txt"), and a cancelled extract leaves nothing
- **Drag files out** into file managers, browsers, chat apps and editors (also on Wayland, see below)
- Double-click a file to open it in its app
- Make new archives (New, Ctrl+N, or drop files on an empty window): zip, 7z, tar.gz, tar.xz,
  tar.zst, with a compression level
- Add files to an open archive (Add, or drop them on the window) and delete entries (Delete).
  Works for zip, tar, tar.gz/xz/zst and 7z. The archive is rewritten as a new copy, read back to
  check it, and only then swapped in, so a failure or Cancel leaves the original untouched
- Follows the active rice live (colours, corners, shadows), or keep one rice's colours (menu → Colours)

## Command line

```
unleash [ARCHIVE…]                 open archives
unleash x [--to DIR] ARCHIVE…      extract each archive next to it (or into DIR)
unleash a [OPTIONS] FILE…          compress files and folders into one archive
    --format zip|7z|tar.gz|tar.xz|tar.zst   (default zip)
    --name NAME   --to DIR   --level fast|normal|best
    --dialog      ask for name, format and place in the New archive window
```

`x` and `a` show a small progress window and return at once (a running Unleash does the
work). This is what Burrow's *Extract here* and *Compress…* call. Extracting is smart: one
folder inside stays that folder, loose files get a folder named after the archive. Nothing is
ever overwritten, and a cancelled job leaves nothing behind.

## How the format is read

| Format | How |
| --- | --- |
| zip (stored/deflate), tar, tar.gz, tar.zst, tar.xz, .gz/.zst/.xz | built in (Rust crates) |
| tar.bz2, tar.lz4, tar.lzma, tar.Z | `bsdtar` (libarchive) |
| 7z, rar, iso, cab, wim, dmg, zip with other methods, … | `7z` (7zip package) |

The format is found from the file's first bytes, not its name.

## Drag and drop on Wayland

Most archive managers drag files out with XDND Direct Save, an X11-only protocol where the drop
target tells the source which folder to extract into. Wayland has nothing like it, so on Hyprland
those drops fail or hand over paths that don't exist yet.

Unleash starts extracting the dragged files the moment the drag starts, into
`~/.cache/unleash/drag/`. It offers them as `text/uri-list`, `application/vnd.portal.filetransfer`
(for Flatpak apps, through the document portal) and plain-text paths. When the target asks for
them, the answer waits until the files are complete. The files stay for a day, because browsers
and chat apps read them while they upload after the drop.

## Install

Needs gtk4, and 7zip / libarchive for the formats listed above.

```sh
./install.sh
```

`./install.sh --uninstall` removes it. Settings live in `~/.config/unleash/config.toml`.

## Test copy

`UNLEASH_APP_ID=world.puppygirl.UnleashTest` runs a separate copy that doesn't hand over to the
one already running.
