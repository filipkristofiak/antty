#!/usr/bin/env bash
# Copy omp session groups for the README demo tapes into DEST. A group is a
# top-level <stem>.jsonl plus its optional <stem>/ subagent directory. Groups
# whose text matches the case-insensitive extended regex in $DEMO_EXCLUDE are
# skipped. DEMO_EXCLUDE is required; set it to "" to copy every group.
# Usage: DEMO_EXCLUDE=REGEX docs/demo/copy-sessions.sh DEST SRC_DIR [GLOB]   (GLOB default: *)
set -euo pipefail
exclude=${DEMO_EXCLUDE?DEMO_EXCLUDE must be set (use "" to copy every session group)}
dest=$1 src=$2 glob=${3:-*}
mkdir -p "$dest"
for f in "$src"/$glob.jsonl; do
  [ -e "$f" ] || continue
  stem=${f%.jsonl}
  if [ -n "$exclude" ]; then
    if grep -qiE -- "$exclude" "$f"; then continue; fi
    if [ -d "$stem" ] && grep -rqiE -- "$exclude" "$stem"; then continue; fi
  fi
  cp "$f" "$dest/"
  if [ -d "$stem" ]; then cp -R "$stem" "$dest/"; fi
done
