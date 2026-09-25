# termleaf

A PDF viewer for the terminal that reloads whenever the file changes.

Run it in a pane next to your editor while you write LaTeX. Every time your build tool (`latexmk -pvc`, texlab's build-on-save, `tectonic`, …) writes the PDF, termleaf redraws it and keeps you on the same page.

## Requirements

termleaf draws pages as real images using the **Kitty graphics protocol**, and it only works in terminals that support it:

- [Kitty](https://sw.kovidgoyal.net/kitty/)
- [Ghostty](https://ghostty.org/)
- [herdr](https://github.com/herdrdev/herdr) running inside one of the above (`terminal.kitty_graphics` is on by default)

In any other terminal, termleaf exits with an error. Sixel and iTerm2 images are not supported yet.

termleaf runs on macOS and Linux.

## Install

With Homebrew on macOS or Linux:

```sh
brew install backyardbit/tap/termleaf
```

With Cargo:

```sh
cargo install termleaf
```

Building from source compiles MuPDF, which needs a C compiler, `make` and libclang. On Linux it also needs `pkg-config` and the fontconfig headers (`libfontconfig-dev` on Debian and Ubuntu). Prebuilt binaries and a shell installer are attached to each [GitHub release](https://github.com/backyardbit/termleaf/releases).

## Usage

```sh
termleaf thesis.pdf
```

The file must exist when termleaf starts. If a later build leaves the PDF broken or half-written, termleaf keeps showing the last good page and marks the status bar `✗ unreadable` until the next good build.

| Key | Action |
| --- | --- |
| `j` / `k` | next / previous page (takes a count, e.g. `5j`) |
| `gg` / `G` | first / last page |
| `<n>G`, `<n>gg`, `:<n>` | go to page *n* |
| `:$` | last page |
| `+` / `-` | zoom in / out (`=` also zooms in; takes a count) |
| `s` / `a` | fit width / fit page |
| `q`, `:q`, `Ctrl-C` | quit |

Pages scroll continuously and open at fit width.

| Mouse | Action |
| --- | --- |
| wheel | scroll |
| shift + wheel, sideways swipe | pan left / right |
| `Ctrl` + wheel | zoom at the pointer |
| drag | pan |
| click | follow a link (`\ref`, citations, table of contents) |
| double-click | switch between fit width and fit page |

### Pinch to zoom

Terminals never pass trackpad pinches to the programs running inside them, so termleaf reads pinches from the operating system itself. It zooms at the pointer while its pane has focus. It needs a broad permission, and until that is granted pinch simply does nothing:

- **macOS:** your terminal app (Ghostty, kitty, …) needs **Input Monitoring** in System Settings → Privacy & Security. macOS asks the first time termleaf starts. Restart the terminal after allowing it. The permission belongs to the terminal app, so every program you run in it could then watch keyboard and mouse input.
- **Linux:** your user must be in the `input` group (`sudo usermod -aG input $USER`, then log in again). termleaf only opens devices that report themselves as touchpads, but the group gives read access to every input device.

Run `termleaf --no-pinch` to keep termleaf away from OS input entirely. Ctrl+wheel and `+`/`-` zoom without any permission.

## License

AGPL-3.0-or-later, matching [MuPDF](https://mupdf.com/), which termleaf uses to render pages.
