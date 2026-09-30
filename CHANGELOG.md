# Changelog

## v0.2.1

- Follow no longer stops after a focus-in that no focus-out follows. termleaf used to treat itself as focused from then on and ignore every editor position after the first, for Neovim, Helix, the Vim snippet and `termleaf --follow`, on every OS. termleaf now trusts focus reports only after it has seen a focus-out (#39).
- Helix follow works on macOS. termleaf couldn't read Helix's working directory there, so it never watched a Helix pane (#39).
- CI runs the follow end-to-end tests on macOS too (#39).

## v0.2.0

### Sixel and iTerm2 images

- Terminals without Kitty graphics now show pages instead of exiting. Sixel covers foot, `xterm -ti vt340`, mlterm and Windows Terminal. iTerm2 inline images cover WezTerm, iTerm2 and Konsole.
- Inside tmux 3.6 or later built with Sixel, termleaf draws Sixel when tmux reports Sixel support. iTerm2 images aren't used inside tmux.
- `--graphics kitty`, `--graphics sixel` or `--graphics iterm2` forces a protocol. The default, `auto`, picks from what the terminal reports, and exits with an error when it reports none. Kitty terminals take the same Kitty path as before.
- Sixel and iTerm2 images are one picture of the page area, sent again on every scroll, zoom and page change. That is slower than Kitty graphics, especially over SSH.

### Fixes

- Sixel and iTerm2: the tile cache stays within its memory budget, and prefetching takes only the tiles that fit (#17).
- iTerm2 images: tiles that arrive after a scroll repaint the page at most every 100 ms (#18), and frames are capped at one every 33 ms (#19).
- Sixel and iTerm2: a jump no longer flashes a blank or half-drawn page (#20), and its visible tiles render before its prefetch tiles (#29).
- With `NO_COLOR` set, kitty and Ghostty showed a blank page. Pages draw again (#27).
- Inside tmux, termleaf asks the tmux that runs the server in `$TMUX`, not the first `tmux` on PATH. A tmux 3.6 server with an older tmux first on PATH is now detected as Sixel, where termleaf used to exit (#30).

### SyncTeX

Build with `-synctex=1`. The README's SyncTeX section has the details.

- Inverse search: `e` (at the pointer, or the middle of the pane), Alt+click or Ctrl+click opens the source line in your editor.
- Follow, on by default: termleaf shows the page with your editor's cursor line. `F`, `:follow` and `:nofollow` switch it, and `--no-follow` starts with it off. `termleaf --follow file.tex:line[:column]` sends a position from scripts and editor keys. It exits 0 even when no termleaf takes the position.
- Editors:
  - Neovim needs no config. It is reached over its RPC socket, in a multiplexer or without one.
  - Helix needs no config. Inverse search types the jump into its pane, and follow reads its statusline through the multiplexer.
  - Vim: inverse search types the jump into its pane. Follow needs `contrib/termleaf.vim`.
- Multiplexers: tmux, herdr, zellij 0.44 or later, GNU screen, kitty windows (with `allow_remote_control` and `listen_on`), and WezTerm panes.
- Each termleaf listens on `$XDG_RUNTIME_DIR/termleaf/<pid>.sock`, or on `${TMPDIR:-/tmp}/termleaf-$USER/<pid>.sock`, in a directory with mode 0700.

Known limits:

- The editor must run in the same multiplexer as termleaf, as your user, on the same machine. Anywhere else (a plain terminal, Konsole, Ghostty), only Neovim works.
- Helix follow needs a multiplexer from the list and Helix's default statusline: a `NOR`, `INS` or `SEL` label, then the file name, with `line:col` last. A file name cut short in a narrow pane, or a statusline covered by the `:` prompt, moves nothing. Where capture is impossible, a Helix 25.07 key can run `termleaf --follow`.
- Neovim is found by its swap file, so a Neovim with `noswapfile` or `nvim -n` isn't followed.
- zellij doesn't pass Alt+click or Ctrl+click on, so use `e` there.
- A path containing shell or editor metacharacters can't be typed into an editor. Only Neovim's RPC is used for it (`path needs RPC`).
- The editor features are tested end to end on Linux. On macOS only the unit tests run.

### Other

- The site is at https://backyardbit.github.io/termleaf/ (#10).
- The Homebrew instructions are gone. The tap never existed, so the `brew install backyardbit/tap/termleaf` line in the v0.1.1 release notes never worked (#10).
