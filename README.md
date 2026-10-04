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

With Homebrew on macOS or Linux:

```sh
brew install backyardbit/tap/termleaf
```

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
| `e` | open the source line under the pointer in your editor ([SyncTeX](#synctex)) |
| `/`, then Enter | search PDF text |
| `n`, `N` | next / previous match, wrapping at the ends (counts work) |
| Escape | cancel a search prompt, or dismiss search results |
| `F`, `:follow`, `:nofollow` | follow your editor on / off ([SyncTeX](#synctex)) |
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
| `Alt` + click, `Ctrl` + click | open the source line under the click in your editor ([SyncTeX](#synctex)) |

### Text search

Press `/`, type a phrase and press Enter. Matches are yellow, with the current match in orange; the status bar shows the query and current/total match count. `n` and `N` move forwards and backwards, wrapping at the ends. A count such as `3n` skips three matches. An empty prompt repeats the last submitted query. Escape cancels an unsubmitted prompt, keeping the previous search; Escape outside the prompt dismisses the search and its highlights. A query with no results says `no matches`.

Search is case-insensitive and uses the PDF's native text, including Unicode and phrases spanning lines. It does not perform OCR: scanned pages need an existing text layer. Search runs in the background a page at a time, behind pending rendering work. The status says `searching…` until it finishes. Results are limited to 10,000 matches, with an explicit limit notice; a text-extraction failure says `search failed`.

Highlights work with Kitty, Sixel and iTerm2 graphics and follow zooming and scrolling. A successful PDF rebuild reruns the query without moving your view; incomplete rebuilds keep the last good PDF and its matches. Search does not turn editor follow off; use `F` when you want to browse independently of editor movement.

### Pinch to zoom

Terminals never pass trackpad pinches to the programs running inside them, so termleaf reads pinches from the operating system itself. It zooms at the pointer while its pane has focus. It needs a broad permission, and until that is granted pinch simply does nothing:

- **macOS:** your terminal app (Ghostty, kitty, …) needs **Input Monitoring** in System Settings → Privacy & Security. macOS asks the first time termleaf starts. Restart the terminal after allowing it. The permission belongs to the terminal app, so every program you run in it could then watch keyboard and mouse input.
- **Linux:** your user must be in the `input` group (`sudo usermod -aG input $USER`, then log in again). termleaf only opens devices that report themselves as touchpads, but the group gives read access to every input device.

Run `termleaf --no-pinch` to keep termleaf away from OS input entirely. Ctrl+wheel and `+`/`-` zoom without any permission.

## SyncTeX

termleaf jumps from the PDF to the source line in your editor (inverse search), and follows your editor's cursor through the PDF (forward search). Both need SyncTeX data. Build with `-synctex=1` (for example `latexmk -pdf -synctex=1 -pvc`), which writes `thesis.synctex.gz` next to `thesis.pdf`. An uncompressed `thesis.synctex` works too. Without it, the status bar says `no SyncTeX data: build with -synctex=1`. If the data is older than the PDF, termleaf still uses it and adds `SyncTeX data is older than the PDF`.

### Inverse search

| Input | Action |
| --- | --- |
| `e` | open the source line under the pointer, or under the middle of the pane, in your editor |
| `Alt` + click | open the source line under the click |
| `Ctrl` + click | the same, where the terminal passes it on |

zellij (tested with 0.45.1) passes neither modifier-click on, so use `e` there.

termleaf looks for Neovim, Vim or Helix in the multiplexer it runs in (see [Editors and multiplexers](#editors-and-multiplexers)). An editor that has the file open wins over one that doesn't. Right before sending, termleaf checks that the pane's foreground process is still that editor, owned by you, with no `Press ENTER`, swap-file or yes/no prompt waiting. It never types into a shell or any other program. The status bar shows the outcome for 4 s:

- `→ nvim in tmux %3 · chapters/intro.tex:12`: sent;
- `chapters/intro.tex:12 · no editor found`: nothing to send to;
- `2 editors could take chapters/intro.tex:12`: a tie, so nothing is sent.

Neovim is told over its RPC socket. Otherwise termleaf types the jump into the pane: `Ctrl-\ Ctrl-N :drop <file> | <line>` for Vim, and `Esc`, then `:open <file>:<line>:1` for Helix, which opens it in the focused view. A path that contains any of ``$ ` ; & < > ( ) ' " | %`` can't be typed, so the status bar says `path needs RPC` and only Neovim's RPC is used.

### Follow

Follow is on by default. As you move through a `.tex` file, termleaf shows the page with your cursor's line.

| Key or flag | Action |
| --- | --- |
| `F` | follow on / off |
| `:follow`, `:nofollow` | follow on, follow off |
| `termleaf --no-follow thesis.pdf` | start with follow off |

The status bar ends with `follow: nvim at chapters/intro.tex:12`, `follow: hx at …`, `follow: chapters/intro.tex:12` (from the Vim snippet or `termleaf --follow`), or `follow off`.

- termleaf scrolls only when the line is on another page, or off screen because you zoomed in. Moving within a page leaves the view alone.
- While termleaf has focus, editor moves are ignored, so they don't fight your own scrolling. Helix moves made meanwhile are caught up once termleaf loses focus.
- Turning follow on shows where the editor is, even while termleaf has focus. That's the newest position it sent while follow was off. Helix is read again, and a Neovim that termleaf connects to for the first time reports its cursor at once.
- A move that arrives during a reload is shown after the reload.
- A position in a file that isn't part of this PDF is ignored, so several termleafs can run side by side.

### Editors and multiplexers

| Editor | Inverse search | Follow | Setup |
| --- | --- | --- | --- |
| Neovim | over its RPC socket, in any multiplexer below or none. In a multiplexer below, typed keys if the socket can't be reached | over its RPC socket, in any multiplexer or none | none |
| Helix | typed keys, in a multiplexer below | reads its statusline through a multiplexer below | none |
| Vim | typed keys, in a multiplexer below | the [Vim snippet](#vim-snippet) | the snippet, for follow only |

| termleaf and the editor in | Inverse search and Helix follow | Needs |
| --- | --- | --- |
| tmux | ✓ | |
| [herdr](https://github.com/herdrdev/herdr) | ✓ | |
| zellij | ✓ | zellij 0.44 or later |
| GNU screen | ✓ | |
| kitty windows | ✓ | `allow_remote_control socket-only` and `listen_on unix:/tmp/kitty` in `kitty.conf`. Otherwise the status bar says `kitty remote control is off`. |
| WezTerm panes | ✓ | `wezterm cli` (tested with `wezterm-mux-server`) |
| anything else (a plain terminal, Konsole, Ghostty, …) | Neovim only | |

The editor has to run in the same multiplexer as termleaf, as your user, on the same machine.

**Neovim** needs no config. Every 2 s termleaf looks for your Neovims that have a swap file for one of the document's files. Over each one's RPC socket, it adds an autocommand that reports the cursor 150 ms after the last move. The autocommand removes itself once termleaf has quit. A Neovim without swap files (`noswapfile`, `nvim -n`) isn't found.

**Helix** has no plugin API yet, so termleaf reads its statusline through the multiplexer. The limits:

- It needs a multiplexer from the table: not a plain terminal, Konsole, Ghostty, or kitty without remote control.
- It reads the default statusline: a `NOR`, `INS` or `SEL` label, then the file name, with `line:col` last on the right. Custom mode labels, or a statusline without the mode, file name or position, aren't read.
- A file name cut short in a narrow pane, or a statusline covered by the `:` prompt, moves nothing.
- Panes are read only while follow is on and termleaf doesn't have focus: every 500 ms, slowing to every 2 s while nothing changes. A Helix started later is found within about 2 s.
- Only the innermost multiplexer that has a Helix is read. With several Helix panes, the latest change wins.

Where the statusline can't be read, a Helix (25.07 or later) key can send the position with `termleaf --follow`:

```toml
# ~/.config/helix/config.toml
[keys.normal.space]
F = ":sh termleaf --follow %{buffer_name}:%{cursor_line}:%{cursor_column}"
```

`Space F` then shows Helix's cursor line in termleaf.

### Vim snippet

Vim has no socket termleaf can reach, so follow needs [`contrib/termleaf.vim`](contrib/termleaf.vim), which the end-to-end tests also use:

```sh
curl -fLo ~/.vim/plugin/termleaf.vim --create-dirs https://raw.githubusercontent.com/backyardbit/termleaf/master/contrib/termleaf.vim
```

In `.tex` buffers, it sends the cursor position to every termleaf 150 ms after the last move, and only when the position changed. `:TermleafFollowToggle`, or `let g:termleaf_follow = 0`, stops it sending. It needs a Vim with `+channel` whose `ch_open()` takes `unix:` addresses (tested with Vim 9.1). Inverse search needs no snippet.

### termleaf --follow

```sh
termleaf --follow chapters/intro.tex:12
termleaf --follow chapters/intro.tex:12:5
```

This shows that line in every running termleaf whose PDF was built from the file, then exits. A relative path is taken from the current directory. It exits 0 even when no termleaf took the position, so an editor hook never shows an error. It is meant for scripts, editor hooks and the Helix key above.

### Socket directory

Each termleaf listens on `$XDG_RUNTIME_DIR/termleaf/<pid>.sock`, or on `${TMPDIR:-/tmp}/termleaf-$USER/<pid>.sock` when `XDG_RUNTIME_DIR` isn't set (as on macOS). termleaf creates the directory and sets it to mode 0700. At startup it removes sockets that nobody listens on, and it removes its own socket on exit, SIGTERM and SIGHUP. `termleaf --follow` sends nothing unless the directory is a real directory, not a symlink, with no group or other permissions. The protocol is one line per connection: `follow <line> <column> <absolute path>`.

SyncTeX's editor features are tested end to end on Linux. On macOS only the unit tests run.

## License

AGPL-3.0-or-later, matching [MuPDF](https://mupdf.com/), which termleaf uses to render pages.
