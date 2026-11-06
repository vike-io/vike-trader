#!/usr/bin/env bash
# archive-map.sh — MEASURE which files exist in FXCM's public candle archive (bash + curl + gzip + awk).
#
#   bash crates/bridges/fxcm/scripts/archive-map.sh OUTDIR            # measure (about 10 minutes)
#   bash crates/bridges/fxcm/scripts/archive-map.sh OUTDIR --dry-run  # print the plan, send nothing
#
# MANUAL, and nothing in CI runs it: it talks to a third party's public CDN. It sends anonymous HTTPS
# GETs only (no credential, no cookie, nothing to a trading venue), at most one every GAP_SECONDS and
# at most MAX_REQUESTS in all. The plan is ordered by value, so a run that hits the cap loses the
# least useful probes; the cap is enforced in `probe`, not promised by the plan.
#
# WHY A SAMPLE AND NOT EVERY WEEK. Two pairs x m1/H1 x ten years is 2 x 2 x 530 candidate files, five
# times the cap. The archive has no listing, so the map is a SAMPLE: seven weeks a year, the year-edge
# weeks for one series, and a short walk from week 1 for the earliest file. A 404 is "this URL has no
# file", never "this week had no candles" (the week may live under another number, see below).
#
# WHAT EACH LINE OF map.tsv SAYS (tab separated):
#   kind     week | year           (m1 and H1 are weekly files, D1 is a yearly file)
#   pair     EURUSD | GBPUSD
#   interval m1 | H1 | D1
#   year     the folder
#   number   the file name (week number), or - for D1
#   http     the status; 000 = no answer
#   bytes    the compressed size
#   first, last   the file's own first and last DateTime (UTC), read from INSIDE the file
#   rows     data rows
#   monday_iso    the ISO year-week of the MONDAY of the trading week the file holds
#   delta    number - ISO week: 0 = the README's rule, -1 = shifted by one (2026), anything else = irregular
#
# THE FILE'S OWN DATES ARE THE TRUTH. FXCM numbers the weeks irregularly (2020..2025 follow the ISO
# week of the Monday, 2026 is one lower), so this script never computes a number to decide what a file
# holds: it reads `first` and `last`.
set -u

BASE="https://candledata.fxcorporate.com"
MAX_REQUESTS=400
GAP_SECONDS="1.2"
PAIRS="EURUSD GBPUSD"
SAMPLE_WEEKS="2 10 18 26 34 42 50"
EDGE_WEEKS="1 52 53"
YEARS="2017 2018 2019 2020 2021 2022 2023 2024 2025 2026"
OLDER_YEARS="2012 2013 2014 2015 2016"
D1_YEARS="2012 2013 2014 2015 2016 2017 2018 2019 2020 2021 2022 2023 2024 2025 2026"
EARLIEST_WALK="1 2 3 4 5 6 7 8"

out="${1:-}"
mode="${2:-}"
if [ -z "$out" ]; then
  echo "usage: archive-map.sh OUTDIR [--dry-run]" >&2
  exit 2
fi
mkdir -p "$out" || exit 1
map="$out/map.tsv"
summary="$out/summary.txt"
body="$out/.body.gz"
requests=0
declare -A seen=()

# The ISO year-week ("2025-W10") of the Monday of the trading week whose first row falls on `$1`
# (YYYY-MM-DD). A Sunday-evening first row belongs to the week that starts the next day.
monday_iso() {
  local d="$1" w
  w="$(date -u -d "$d" +%u 2>/dev/null)" || { echo "-"; return; }
  if [ "$w" = "7" ]; then
    date -u -d "$d +1 day" +%G-W%V
  else
    date -u -d "$d -$((w - 1)) days" +%G-W%V
  fi
}

# probe KIND PAIR INTERVAL YEAR NUMBER — one GET (never a repeat of a URL this run already asked).
probe() {
  local kind="$1" pair="$2" iv="$3" year="$4" num="$5" url key
  if [ "$kind" = "year" ]; then
    url="$BASE/$iv/$pair/$year.csv.gz"
  else
    url="$BASE/$iv/$pair/$year/$num.csv.gz"
  fi
  key="$url"
  [ -n "${seen[$key]:-}" ] && return 0
  if [ "$requests" -ge "$MAX_REQUESTS" ]; then
    echo "CAP: $MAX_REQUESTS requests reached; not asking $url" >> "$summary"
    return 1
  fi
  seen[$key]=1
  if [ "$mode" = "--dry-run" ]; then
    requests=$((requests + 1))
    echo "plan $requests: $url"
    return 0
  fi
  sleep "$GAP_SECONDS"
  requests=$((requests + 1))
  local res http bytes
  res="$(curl -sS -m 40 -o "$body" -w '%{http_code} %{size_download}' "$url" 2>/dev/null)" || res="000 0"
  http="${res%% *}"
  bytes="${res##* }"
  local first="-" last="-" rows="-" mon="-" delta="-"
  if [ "$http" = "200" ]; then
    local text
    if text="$(gzip -dc "$body" 2>/dev/null | tr -d '\r' | awk -F, 'NR==2{f=$1} NR>1{l=$1; n++} END{print f "|" l "|" n+0}')"; then
      first="${text%%|*}"
      text="${text#*|}"
      last="${text%%|*}"
      rows="${text##*|}"
    fi
    if [ "$kind" = "week" ] && [ "$first" != "-" ] && [ -n "$first" ]; then
      # `MM/DD/YYYY HH:MM:SS.mmm` -> `YYYY-MM-DD`
      local d="${first:6:4}-${first:0:2}-${first:3:2}"
      mon="$(monday_iso "$d")"
      local isow="${mon##*-W}"
      [ "$isow" != "-" ] && delta=$((num - 10#$isow))
    fi
  fi
  printf '%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\n' \
    "$kind" "$pair" "$iv" "$year" "$num" "$http" "$bytes" "$first" "$last" "$rows" "$mon" "$delta" >> "$map"
  echo "[$requests/$MAX_REQUESTS] $http $url ${first} .. ${last}"
  return 0
}

: > "$summary"
if [ "$mode" != "--dry-run" ]; then
  printf 'kind\tpair\tinterval\tyear\tnumber\thttp\tbytes\tfirst\tlast\trows\tmonday_iso\tdelta\n' > "$map"
fi

# 1. D1: one yearly file per pair and year.
for pair in $PAIRS; do
  for year in $D1_YEARS; do probe year "$pair" D1 "$year" - || break; done
done
# 2. The coarse weekly sample, 2017..2026.
for iv in m1 H1; do
  for pair in $PAIRS; do
    for year in $YEARS; do
      for wk in $SAMPLE_WEEKS; do probe week "$pair" "$iv" "$year" "$wk" || break 3; done
    done
  done
done
# 3. Is there anything before 2017? One mid-March week per older year.
for iv in m1 H1; do
  for pair in $PAIRS; do
    for year in $OLDER_YEARS; do probe week "$pair" "$iv" "$year" 10 || break 3; done
  done
done
# 4. The year-edge weeks of one series, where the numbering is least regular.
for year in $YEARS; do
  for wk in $EDGE_WEEKS; do probe week EURUSD m1 "$year" "$wk" || break 2; done
done
# 5. The earliest file: walk weeks 1.. of the earliest year that answered, per series.
if [ "$mode" != "--dry-run" ]; then
  for iv in m1 H1; do
    for pair in $PAIRS; do
      y0="$(awk -F'\t' -v p="$pair" -v i="$iv" '$1=="week" && $2==p && $3==i && $6==200 {print $4}' "$map" | sort -n | head -1)"
      [ -z "$y0" ] && continue
      for wk in $EARLIEST_WALK; do probe week "$pair" "$iv" "$y0" "$wk" || break 3; done
    done
  done
fi

if [ "$mode" = "--dry-run" ]; then
  echo "dry run: $requests planned requests of at most $MAX_REQUESTS (the earliest-file walk is planned from the answers and is not counted)"
  exit 0
fi

{
  echo "requests sent: $requests (cap $MAX_REQUESTS), gap $GAP_SECONDS s"
  echo
  echo "== D1 (yearly files): years that answered 200, per pair"
  awk -F'\t' '$1=="year" && $6==200 {y[$2]=y[$2] " " $4} END{for (p in y) print p ":" y[p]}' "$map" | sort
  echo
  echo "== weekly files: per pair, interval and year, numbers that answered 200 / numbers asked"
  awk -F'\t' '$1=="week" {k=$2 " " $3 " " $4; a[k]=a[k] " " $5 (($6==200)?"+":"-")} END{for (k in a) print k ":" a[k]}' "$map" | sort
  echo
  echo "== earliest weekly file that answered, per pair and interval (smallest year, then number)"
  awk -F'\t' '$1=="week" && $6==200 {k=$2 " " $3; if (!(k in y) || $4<y[k] || ($4==y[k] && $5+0<n[k]+0)) {y[k]=$4; n[k]=$5; f[k]=$8}} END{for (k in y) print k ": " y[k] "/" n[k] "  first row " f[k]}' "$map" | sort
  echo
  echo "== numbering: delta = number - ISO week of the Monday, per year (200s only)"
  awk -F'\t' '$1=="week" && $6==200 && $12!="-" {c[$4 " delta=" $12]++} END{for (k in c) print k ": " c[k] " files"}' "$map" | sort
  echo
  echo "== URLs that answered something other than 200 (a 404 here is NOT proof of no candles)"
  awk -F'\t' '$6!=200 {print $6 "\t" $2 "\t" $3 "\t" $4 "\t" $5}' "$map" | sort | uniq -c | sort -rn | head -80
} >> "$summary"
rm -f "$body"
echo "wrote $map and $summary"
