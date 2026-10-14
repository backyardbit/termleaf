#!/bin/sh
# Runs the probe matrix in the current terminal. Env: PROBE_OUT PROBE_LABEL PROBE_REPS PROBE_SECONDS
# Densities: retina/d075 = one image per page; retina-bands/d075-bands = each page split into <=16 MiB bands
# (herdr >= 0.9.2 sends images <= 16 MiB raw to local Ghostty as temp files, larger ones inline).
set -eu
OLDPWD_E=$PWD; cd "$(dirname "$0")"
r=1
while [ "$r" -le "$PROBE_REPS" ]; do
  for d in retina d075 retina-bands d075-bands; do
    case $d in
      *-bands) b=${d%-bands}; pages=$(ls $b-100-b*.png $b-101-b*.png | tr '\n' ' ');;
      *) pages="$d-100.png $d-101.png";;
    esac
    for s in trackpad held_j held_j_upload; do
      python3 probe.py --pages $pages --scenario "$s" --seconds "$PROBE_SECONDS" \
        --density "$d" --label "$PROBE_LABEL" --out "$OLDPWD_E/$PROBE_OUT" || echo "probe failed: $s $d rep $r" >&2
      sleep 0.3
    done
  done
  r=$((r + 1))
done
