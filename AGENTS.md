# termleaf

A terminal PDF viewer, written in Rust, that live-reloads the PDF whenever it changes on disk.

The main use case is writing LaTeX. Helix runs in one herdr pane and termleaf runs in the pane next to it, showing the compiled PDF. Each rebuild appears in termleaf without any input from the user. Every design decision should serve that loop.

## Direction

- **Viewer, not build system.** termleaf watches a PDF and redraws when it changes. It works with any build tool (`latexmk -pvc`, texlab's build-on-save, `tectonic`). Any compile integration added later must be optional and live at the edge of the program.
- **Only an existing PDF can be opened.** `termleaf file.pdf` exits with an error if the file is missing or can't be read at startup.
- **One page at a time**, sized to fit the pane. Continuous scrolling is a later mode.
- **Reload never loses your place.** Keep the current page across a reload. If the document gets shorter, clamp to the last page.
- **Reload handles half-written files.** Build tools truncate and rewrite the PDF, or replace it with a renamed file, so:
  - Watch the directory, not the file.
  - Debounce file events.
  - Treat a file that doesn't end with `%%EOF` as incomplete.
  - Retry when parsing fails.
  - Keep showing the last good page, and mark the status bar `✗ unreadable`.
- **The UI never blocks.** Pages render on the render thread. Keys stay responsive while a page renders, and neighbouring pages are prefetched.
- **Rendering uses MuPDF** through the `mupdf` crate, with its default features. MuPDF is AGPL, so termleaf is AGPL-3.0-or-later.
  - `Document`, `Page` and `Pixmap` are not `Send`. Every MuPDF object lives on the render thread in `renderer.rs`, and only plain images cross threads.
- **Kitty graphics protocol only.** Drawing goes through `ratatui` + `ratatui-image` (Unicode placeholders). In any other terminal, termleaf exits with a clear error. There is no text-block fallback, and the README says so.
- **herdr on Ghostty is the primary target.** Facts about herdr:
  - It re-sends Kitty images to the outer terminal and supports Unicode placeholders.
  - It blanks the image for about 20 ms each time an image is re-sent (herdr#3676). Send each rendered page once and keep its protocol object cached.
  - It silently drops graphics frames larger than about 32 MB.
  - It reports cell pixel size through `CSI 16 t` and `TIOCGWINSZ`, but only after its first resize.
- **Keyboard only, in the style of vim.**
  - `j` goes to the next page and `k` goes to the previous page.
  - Other bindings: `gg`/`G`, count prefixes (`5j`), `:<n>` to jump to a page, `q` to quit.
  - All bindings live in `keys.rs` so they can later be configured.
- **Small and fast.** One binary, quick startup, few dependencies. Adding a dependency needs a reason.
- **Platforms:** macOS and Linux.

## Later

These are wanted eventually. Keep the architecture open to them, but do not build them early:

- Continuous scrolling
- SyncTeX forward and inverse search between Helix and termleaf
- Zoom and fit modes (fit width, fit page)
- A dark mode that inverts or recolours pages
- A config file for keybindings and defaults
- Sixel and iTerm2 protocols, and a Homebrew tap

## Working in this repo

- **Use test-driven development.** Write a failing test that states the behaviour you want, watch it go red, then write the code that turns it green. A test written after the code tends to restate what the code does rather than what it should do. Each test names a behaviour a user or caller relies on, not an implementation detail.
- A change is done when `cargo xtask check` passes. It runs fmt, clippy, the tests and `cargo xtask lint`. CI runs the same check on `ubuntu-latest` and `macos-latest`.
- The real-terminal end-to-end test, `cargo xtask e2e` (the `e2e` job in `ci.yml`), runs only in GitHub Actions and refuses to run without `CI` set. It uses Ghostty + herdr on a virtual display, drives termleaf by keys and checks it by screenshot. Never run it, or anything else that opens windows or captures the screen, on the owner's machines. To check real-terminal behaviour, push and read the CI results and their screenshot artifacts.
- The lint rules are a Rust port of the anti-slop Oxlint rules. Clippy settings are in `Cargo.toml` and `clippy.toml`, and the custom checks are in `xtask/`. Keep the explanation of the code in names, types, small functions and tests:
  - Write no comments of any kind, doc comments included. The one exception is `// SAFETY:`, which every `unsafe` block needs.
  - Parse external input into a domain type at its boundary. `dyn Any` and lossy `as` casts are denied. Convert with `From`/`TryFrom`.
  - To suppress a lint, use `#[expect(lint, reason = "...")]`. `#[allow]` is denied.
  - In tests, replace a dependency through a real trait and a fake implementation of it. Mocking crates are banned.
  - No identifier may contain "shape". Name things after their domain role.
- Test PDFs live in `tests/fixtures/`, each committed next to the `.tex` it was compiled from. Tests build broken variants (cut short, empty, missing) from those files in code.
- Tests that touch the filesystem watcher check that events arrive, never that they don't. macOS FSEvents delivers events late and in batches.
- Build gotchas:
  - `unicode-ident` is pinned in `xtask/Cargo.toml`. `ra-ap-rustc_lexer` fails to compile when its Unicode version differs from `unicode-properties`.
  - The MuPDF build fails on macOS when Homebrew's GNU Make 4.x comes first in PATH (mupdf-rs#211).
- Releases: bump `version` in `Cargo.toml`, then push a `v*` tag. The `dist` workflow builds the binaries and a shell installer, and `cargo publish` pushes the crate to crates.io. `xtask` is never published.
- Commit `Cargo.lock`. This is a binary crate.
- The human owner is the only author of every commit. Write commit messages with no `Co-Authored-By` trailer and no other attribution to an agent or AI tool. The same goes for PR descriptions.
