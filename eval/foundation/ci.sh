#!/bin/bash
# Foundation eval on a GitHub macOS runner (throwaway). Called by .github/workflows/foundation-eval.yml.
# Needs: ./bin/termleaf-a ./bin/termleaf-b (built by the workflow), pdfs/, herdr on PATH, Ghostty.app (optional).
# Legs: ghostty check -> A/B hold_j under herdr -> probe under herdr -> probe direct in Ghostty.
# If Ghostty can't open a window, every herdr leg runs with host.py (no drawing) as the host terminal instead,
# and the direct probe is skipped; results/mode.txt says which.
set -u
cd "$(dirname "$0")"; E=$PWD
REPS=${REPS:-10}; PREPS=${PROBE_REPS:-3}
R=$E/results; mkdir -p "$R"
GAPP=${GHOSTTY_APP:-/Applications/Ghostty.app}
export XDG_CONFIG_HOME=/tmp/fe/config XDG_STATE_HOME=/tmp/fe/state XDG_DATA_HOME=/tmp/fe/data XDG_CACHE_HOME=/tmp/fe/cache
HERDR=$(command -v herdr)
{ echo "date: $(date -u)"; sw_vers; sysctl -n machdep.cpu.brand_string hw.ncpu; herdr --version
  "$GAPP/Contents/MacOS/ghostty" +version 2>&1 | head -3; system_profiler SPDisplaysDataType 2>/dev/null | head -20; } > "$R/env.txt" 2>&1
GOPTS=(--window-width=118 --window-height=29 --font-size=22 --confirm-close-surface=false --quit-after-last-window-closed=true)

# 1. can Ghostty open a window and run a command here?
rm -f /tmp/fe-gh-ok
open -na "$GAPP" --args "${GOPTS[@]}" -e /bin/sh -c "python3 $E/winsize.py /tmp/fe-gh-ok; sleep 2" >"$R/ghostty-open.log" 2>&1
for _ in $(seq 60); do [ -s /tmp/fe-gh-ok ] && break; sleep 1; done
if [ -s /tmp/fe-gh-ok ]; then MODE=ghostty; cp /tmp/fe-gh-ok "$R/ghostty-winsize.txt"; else MODE=hostpy; fi
echo "$MODE" > "$R/mode.txt"; echo "== mode: $MODE"
pgrep -fl ghostty >> "$R/ghostty-open.log" 2>&1; log show --last 2m --predicate 'process == "ghostty"' --style compact 2>/dev/null | tail -40 >> "$R/ghostty-open.log"
screencapture -x "$R/screen-after-ghostty-check.png" 2>>"$R/ghostty-open.log" || true

start_herdr() { # $1 = tag for host accounting
  pkill -x herdr 2>/dev/null; rm -rf /tmp/fe; mkdir -p /tmp/fe/{config/herdr,state,data,cache}
  printf 'onboarding = false\n' > /tmp/fe/config/herdr/config.toml
  if [ "$MODE" = ghostty ]; then open -na "$GAPP" --args "${GOPTS[@]}" -e "$HERDR" --session febench
  else python3 "$E/host.py" "$R/host-$1.jsonl" 118 29 22 61 -- "$HERDR" --session febench > /dev/null 2>&1 & HPID=$!; fi
  for _ in $(seq 60); do "$HERDR" --session febench pane list >/dev/null 2>&1 && break; sleep 1; done; sleep 3
  PANE=$("$HERDR" --session febench pane list | python3 -c "import json,sys; print(json.load(sys.stdin)['result']['panes'][0]['pane_id'])")
}
stop_herdr() { "$HERDR" --session febench server stop >/dev/null 2>&1; sleep 2; pkill -x ghostty 2>/dev/null; [ -n "${HPID:-}" ] && kill "$HPID" 2>/dev/null; HPID=; }
waitf() { local end=$((SECONDS + $2)); until [ -f "$1" ]; do [ $SECONDS -ge $end ] && { echo "timeout waiting for $1"; return 1; }; sleep 3; done; }
cpu() { ( while [ ! -f "$1" ]; do date +%s; ps -axo pid=,%cpu=,rss=,command= | awk '$4 ~ /herdr|ghostty|termleaf/'; sleep 1; done ) > "$2" & }

# 2. A/B hold_j under herdr (same harness as run-mac-ab; Mac pane geometry forced)
cat > "$E/ab-pane.sh" <<P
#!/bin/bash
cd $E
EX="--force-size --cols 118 --rows 29 --cell-px 22x61"
run() { python3 driver.py --label "ab-\$1" --binary "bin/termleaf-\$(echo \$1 | tr AB ab)" --mode proxy --scenarios hold_j --max-load 1000 --only "\$2" --extra "\$EX" "\${@:3}"; }
python3 winsize.py results/ab-pane-winsize.txt
i=0
for rep in \$(seq 1 $REPS); do for pdf in progit pgfplots; do i=\$((i+1)); if [ \$((i % 2)) = 1 ]; then o="A B"; else o="B A"; fi
  for b in \$o; do run \$b \$pdf --reps 1 --rep-start \$rep; done; done; done
for pdf in progit pgfplots; do for b in A B; do run \$b \$pdf --reps 0 --trace-rep --rep-start $((REPS + 1)); done; done
touch results/ab.done
P
chmod +x "$E/ab-pane.sh"; rm -f "$R/ab.done"
start_herdr ab; cpu "$R/ab.done" "$R/cpu-ab.txt"
"$HERDR" --session febench pane run "$PANE" "$E/ab-pane.sh; exit" >/dev/null
waitf "$R/ab.done" 2400; stop_herdr
mv runs/ab-A runs/ab-B "$R/" 2>/dev/null
python3 summarize_ab.py "$R/ab" > "$R/ab-summary.md" 2>&1

# 3. probe under herdr
rm -f "$R/probe-herdr.done"
start_herdr probe; cpu "$R/probe-herdr.done" "$R/cpu-probe-herdr.txt"
"$HERDR" --session febench pane run "$PANE" "cd $E; python3 winsize.py results/probe-herdr-winsize.txt; PROBE_OUT=results/probe-herdr.jsonl PROBE_LABEL=herdr PROBE_REPS=$PREPS PROBE_SECONDS=3 ./probe/matrix.sh 2>results/probe-herdr.err; touch results/probe-herdr.done; exit" >/dev/null
waitf "$R/probe-herdr.done" 900; stop_herdr

# 4. probe directly in Ghostty
if [ "$MODE" = ghostty ]; then
  rm -f "$R/probe-direct.done"; cpu "$R/probe-direct.done" "$R/cpu-probe-direct.txt"
  open -na "$GAPP" --args "${GOPTS[@]}" -e /bin/bash -c "cd $E; python3 winsize.py results/probe-direct-winsize.txt; PROBE_OUT=results/probe-direct.jsonl PROBE_LABEL=direct PROBE_REPS=$PREPS PROBE_SECONDS=3 ./probe/matrix.sh 2>results/probe-direct.err; touch results/probe-direct.done"
  waitf "$R/probe-direct.done" 900; pkill -x ghostty 2>/dev/null
fi
python3 probe/summarize_probe.py "$R" > "$R/probe-summary.md" 2>&1
cat "$R/mode.txt" "$R/ab-summary.md" "$R/probe-summary.md"
