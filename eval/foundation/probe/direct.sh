#!/bin/sh
# Launched by run.sh in a new Ghostty window (no herdr). Args: <package dir> <reps> <seconds>
cd "$1" || exit 1
{ echo "direct window: TERM_PROGRAM=${TERM_PROGRAM:-} HERDR_ENV=${HERDR_ENV:-} $(python3 winsize.py /dev/stdout)"; } > runs/direct-env.txt 2>&1
python3 winsize.py runs/direct-winsize.txt
PROBE_OUT=runs/probe-direct.jsonl PROBE_LABEL=direct PROBE_REPS=$2 PROBE_SECONDS=$3 ./probe/matrix.sh > runs/direct-log.txt 2>&1
touch runs/probe-direct.done
