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

## License

AGPL-3.0-or-later, matching [MuPDF](https://mupdf.com/), which termleaf uses to render pages.
