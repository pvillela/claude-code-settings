#!/bin/bash
# Claude Code status line: model name, context % (with progress bar), API cost,
# 5h session limit %, 7d weekly limit %, session name, and elapsed session time

input=$(cat)

model=$(echo "$input" | jq -r '.model.display_name')
effort=$(echo "$input" | jq -r '.effort.level // empty')
cost=$(echo "$input" | jq -r '.cost.total_cost_usd // empty')
used=$(echo "$input" | jq -r '.context_window.used_percentage // empty')
five_hour=$(echo "$input" | jq -r '.rate_limits.five_hour.used_percentage // empty')
seven_day=$(echo "$input" | jq -r '.rate_limits.seven_day.used_percentage // empty')
duration_ms=$(echo "$input" | jq -r '.cost.total_duration_ms // empty')
session_name=$(echo "$input" | jq -r '.session_name // empty')

# Build a 10-segment progress bar for context usage
bar=""
if [ -n "$used" ]; then
  filled=$(awk -v v="$used" 'BEGIN { printf "%.0f", v / 10 }')
  [ "$filled" -gt 10 ] && filled=10
  [ "$filled" -lt 0 ] && filled=0
  empty=$((10 - filled))
  bar="["
  for _ in $(seq 1 "$filled"); do bar="${bar}#"; done
  for _ in $(seq 1 "$empty"); do bar="${bar}-"; done
  bar="${bar}]"
fi

# Format elapsed session time as e.g. "1h 23m" (falls back to "Nm" or "Ns")
elapsed=""
if [ -n "$duration_ms" ]; then
  total_secs=$(awk -v v="$duration_ms" 'BEGIN { printf "%.0f", v / 1000 }')
  hours=$((total_secs / 3600))
  mins=$(((total_secs % 3600) / 60))
  secs=$((total_secs % 60))
  if [ "$hours" -gt 0 ]; then
    elapsed="${hours}h ${mins}m"
  elif [ "$mins" -gt 0 ]; then
    elapsed="${mins}m"
  else
    elapsed="${secs}s"
  fi
fi

parts=()
parts+=("$(printf '\033[1;96m%s\033[0m' "$model")")

if [ -n "$effort" ]; then
  parts+=("$(printf '\033[1;91mThinking: %s\033[0m' "$effort")")
fi

if [ -n "$used" ]; then
  parts+=("$(printf '\033[1;93mCtx: %s %.0f%%\033[0m' "$bar" "$used")")
fi

if [ -n "$cost" ]; then
  parts+=("$(printf '\033[1;95mCost: $%.4f\033[0m' "$cost")")
fi

if [ -n "$five_hour" ]; then
  parts+=("$(printf '\033[1;92m5h: %.0f%%\033[0m' "$five_hour")")
fi

if [ -n "$seven_day" ]; then
  parts+=("$(printf '\033[1;94m7d: %.0f%%\033[0m' "$seven_day")")
fi

if [ -n "$session_name" ]; then
  parts+=("$(printf '\033[1;38;5;208m%s\033[0m' "$session_name")")
fi

if [ -n "$elapsed" ]; then
  parts+=("$(printf '\033[1;97mTime: %s\033[0m' "$elapsed")")
fi

out=""
for p in "${parts[@]}"; do
  if [ -z "$out" ]; then
    out="$p"
  else
    out="$out | $p"
  fi
done
printf '%s\n' "$out"
