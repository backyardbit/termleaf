# Foundation eval (scratch branch, throwaway; never merge)

`.github/workflows/foundation-eval.yml` runs on macos-14 when this branch is pushed (or dispatched):
- installs Ghostty (cask) and herdr 0.9.4 (sha256-pinned release binary);
- builds A = cf54baf + trace.patch and B = master 70bbc34 + trace-master.patch;
- runs `ci.sh`, then uploads `results/` as the `foundation-results` artifact.

`ci.sh` first checks whether Ghostty can open a window and run a command (`results/mode.txt`).
- **ghostty mode:** hold_j A/B (run-mac-ab harness, Mac pane geometry forced) and the resident-page probe run in
  a herdr pane inside Ghostty. The probe then runs again directly in Ghostty.
- **hostpy mode:** used when Ghostty can't open a window. `host.py` stands in for the terminal: it answers
  herdr's queries, consumes t=t files and counts relay bytes per second, but draws nothing. The numbers then
  cover termleaf and herdr only, and the direct probe is skipped.

The probe metric is a proxy (DA1 ack within 16.7 ms, no Kitty error); see probe/probe.py.
Runner caveat: GitHub macOS runners are VMs with a paravirtual GPU, not an M-series display. Even in ghostty
mode, Ghostty's frame timing there is not a Mac's.
