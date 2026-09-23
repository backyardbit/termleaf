# termleaf

A terminal PDF viewer, written in Rust, that live-reloads the PDF whenever it changes on disk.

The main use case is writing LaTeX: Helix runs in one multiplexer pane (tmux, zellij, …) and termleaf runs in the pane next to it, showing the compiled PDF. Each rebuild shows up in termleaf without any input from the user. Every design decision should serve that loop.

## Direction

- **Viewer, not build system.** termleaf watches a PDF file. Compiling is a separate tool's job (`latexmk -pvc`, texlab's build-on-save, `tectonic --watch`). Any compile integration added later must be optional and live at the edge of the program.
- **Reload never loses your place.** Keep the current page, zoom and scroll offset across a reload. If the document gets shorter, clamp to the last page.
- **Reload handles half-written files.** LaTeX tools truncate and rewrite the PDF in place, so the watcher sees partial files. Debounce file events, retry when parsing fails, and keep showing the last good render until a new one succeeds. termleaf must never crash or go blank because it read a file mid-write.
- **The UI never blocks.** Rasterize pages off the input/UI thread. Keys stay responsive while a page renders. Cache nearby pages so paging feels instant.
- **Rendering uses MuPDF** through the `mupdf` crate. MuPDF is AGPL, and so is termleaf: the project is open source under AGPL-3.0-or-later.
- **Terminal graphics come first.** Draw real page images through the Kitty graphics protocol, Sixel or iTerm2 inline images, chosen by detecting what the terminal supports. Fall back to Unicode half-blocks.
- **herdr is the main multiplexer.** The owner runs termleaf in herdr, so herdr is the primary test target. tmux and zellij come after it.
  - herdr passes through Kitty graphics only, and only when `terminal.kitty_graphics` is enabled. It does not pass through Sixel.
  - Detect herdr with `HERDR_ENV=1`. Inside herdr, the outer terminal's variables (such as `GHOSTTY_RESOURCES_DIR`) leak through, so they say nothing about whether graphics will work.
  - Delete the old image placement before drawing a new one. If placements are left behind, herdr shows ghost images when the page redraws.
- **Keyboard only, in the style of vim.**
  - `j` goes to the next page and `k` goes to the previous page.
  - Other bindings follow vim conventions: `gg`/`G`, count prefixes (`5j`), `:<n>` to jump to a page, `q` to quit.
  - Keep bindings in one table so they can later be configured.
- **Small and fast.** One binary, quick startup, few dependencies. Adding a dependency needs a reason.

## Later

These are wanted eventually. Keep the architecture open to them, but do not build them early:

- SyncTeX forward and inverse search between Helix and termleaf
- Zoom and fit modes (fit width, fit page)
- A dark mode that inverts or recolors pages
- A config file for keybindings and defaults

## Working in this repo

- A change is done when `cargo xtask check` passes. It runs fmt, clippy, the tests and `cargo xtask lint`.
- The lint rules are a Rust port of the anti-slop Oxlint rules. Clippy settings are in `Cargo.toml` and `clippy.toml`, and the custom checks are in `xtask/`. Keep the explanation of the code in names, types, small functions and tests:
  - Write no comments of any kind, doc comments included. The one exception is `// SAFETY:`, which every `unsafe` block needs.
  - Parse external input into a domain type at its boundary. `dyn Any` and lossy `as` casts are denied. Convert with `From`/`TryFrom`.
  - To suppress a lint, use `#[expect(lint, reason = "...")]`. `#[allow]` is denied.
  - In tests, replace a dependency through a real trait and a fake implementation of it. Mocking crates are banned.
  - No identifier may contain "shape". Name things after their domain role.
- Put reload and parsing edge cases (partial file, deleted file, shrinking page count) into tests with fixture PDFs. Do not rely on checking them by hand.
- Commit `Cargo.lock`. This is a binary crate.
