# termleaf

A PDF viewer for the terminal that reloads whenever the file changes.

Run it in a pane next to your editor while you write LaTeX. Every time your build tool (`latexmk -pvc`, texlab's build-on-save, `tectonic`, …) writes the PDF, termleaf redraws it and keeps you on the same page.

## Requirements

termleaf draws pages as real images, so it needs a terminal that can show them. It picks the protocol from what the terminal reports:

| Terminal | Protocol | Scrolling |
| --- | --- | --- |
| [Kitty](https://sw.kovidgoyal.net/kitty/) | Kitty graphics | smooth |
| [Ghostty](https://ghostty.org/) | Kitty graphics | smooth |
| [herdr](https://github.com/herdrdev/herdr) inside Kitty or Ghostty (`terminal.kitty_graphics` is on by default) | Kitty graphics | smooth |
| [foot](https://codeberg.org/dnkl/foot), `xterm -ti vt340`, mlterm, Windows Terminal (from WSL or SSH) | Sixel | repaints the page area |
| [tmux](https://github.com/tmux/tmux) 3.6 or later built with Sixel, inside a Sixel terminal | Sixel | repaints the page area |
| [WezTerm](https://wezterm.org/), [iTerm2](https://iterm2.com/), Konsole | iTerm2 images | repaints the page area |

With Kitty graphics, each piece of a page is sent to the terminal once, and scrolling only moves it, so scrolling stays smooth even under herdr. Sixel and iTerm2 images are one picture of the whole page area, so every scroll, zoom and page change sends that picture again. That is slower, and over SSH it is much slower. iTerm2 images don't work inside tmux.

`--graphics kitty`, `--graphics sixel` or `--graphics iterm2` uses that protocol whatever the terminal reports. In a terminal with none of them, termleaf exits with an error. There is no text-block fallback.

termleaf runs on macOS and Linux.

## Install

With the shell installer on macOS or Linux:

```sh
curl --proto '=https' --tlsv1.2 -LsSf https://github.com/backyardbit/termleaf/releases/latest/download/termleaf-installer.sh | sh
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
