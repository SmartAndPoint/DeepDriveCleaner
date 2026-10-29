#!/bin/bash
# Builds load trees on a disposable APFS disk image (story "Synthetic load trees:
# large, wide, deep and hostile", DDC-005): millions of entries never land on the
# user's volume as entries, and one detach removes them. Needs no sudo and raises
# no permission prompt.
#
#   scripts/load-image.sh <profile[,profile...]|all> <S|M|L|XL> [seed]
#
# Every profile is built with `ddc-loadgen build --verify --brief` in its own folder
# on one image: a line per profile gives its build and scan times, and the image's
# size on disk is printed at the end. A failed profile fails the run after every
# profile has been tried. Outputs land in $LOAD_OUT (target/load).
#
# With $LOAD_CLI set, every profile goes through `ddc-loadgen run` instead (story
# "Memory and CPU under load: peak memory, leaks and CPU time"): the tree is built
# and checked the same way, then that CLI is measured on it ($LOAD_REPEAT runs of
# each reading command, one clean), judged against bench/load-limits.tsv when
# $LOAD_MODE is enforce. Each profile's rows land in $LOAD_OUT/<size>-<profile>.tsv,
# all of them in $LOAD_OUT/load-<size>.tsv.
#
# With $LOAD_HEAP set instead, it names a CLI built with ddc-core/dhat-heap, and every
# profile goes through `ddc-loadgen heap`: one scan, its heap profile in
# $LOAD_OUT/dhat-<size>-<profile>.json (`make heap-profile`).
#
# $LOAD_ENTRIES builds fewer entries than the size, on the size's image, and
# $LOAD_START gives the load average from before a series of runs (story
# "Complexity guard: operation counts and scaling exponents": `make scaling`
# measures each profile at an eighth, a quarter, half and all of the size).
set -euo pipefail

usage="usage: load-image.sh <profile[,profile...]|all> <S|M|L|XL> [seed]"
profile=${1:?$usage}
size=${2:?$usage}
seed=${3:-1}
loadgen=${LOADGEN:-target/release/ddc-loadgen}
out=${LOAD_OUT:-target/load}

# A sparse bundle grows with what is written; the cap only bounds it. It builds
# 10-20 times faster than a single-file sparse image or a UDIF image (measured at S).
case $size in
  S) entries=10000 cap=4g ;;
  M) entries=100000 cap=32g ;;
  L) entries=1000000 cap=128g ;;
  XL) entries=10000000 cap=512g ;;
  *) echo "load-image: the size is S, M, L or XL, not $size" >&2; exit 2 ;;
esac
if [ -n "${LOAD_ENTRIES:-}" ]; then
  case $LOAD_ENTRIES in
    '' | *[!0-9]*) echo "load-image: LOAD_ENTRIES is a number, not $LOAD_ENTRIES" >&2; exit 2 ;;
  esac
  if [ "$LOAD_ENTRIES" -lt 1 ] || [ "$LOAD_ENTRIES" -gt "$entries" ]; then
    echo "load-image: LOAD_ENTRIES is 1 to $entries for $size, not $LOAD_ENTRIES" >&2
    exit 2
  fi
  entries=$LOAD_ENTRIES
fi
known=$("$loadgen" profiles | tr '\n' ' ')
if [ "$profile" = all ]; then
  profiles=$known
else
  profiles=
  for p in ${profile//,/ }; do
    case " $known" in
      *" $p "*) profiles="$profiles $p" ;;
      *) echo "load-image: no profile $p; one of: all $known" >&2; exit 2 ;;
    esac
  done
fi

tmp=$(getconf DARWIN_USER_TEMP_DIR)
tmp=${tmp%/}

# The whole disk of the attached image at $1, read from one `hdiutil info`, so an
# image attached or detached meanwhile cannot shift the index; fails when no image
# there is attached.
disk_of() {
  local info i path
  info=$(hdiutil info -plist)
  i=0
  while path=$(plutil -extract "images.$i.image-path" raw -o - - <<<"$info" 2>/dev/null); do
    if [ "$path" = "$1" ]; then
      plutil -extract "images.$i.system-entities.0.dev-entry" raw -o - - <<<"$info"
      return
    fi
    i=$((i + 1))
  done
  return 1
}

# Detaches the image at $1 if it is attached, then deletes it. When the detach
# fails the image is kept, so a later run's sweep still finds it.
discard() {
  local disk
  if disk=$(disk_of "$1"); then
    if ! hdiutil detach -force -quiet "$disk"; then
      echo "load-image: could not detach $disk; $1 is kept for the next run" >&2
      return 1
    fi
  fi
  rm -rf "$1"
}

# hdiutil now and then answers "Resource busy" on a hosted runner.
retry() {
  local n
  for n in 1 2 3; do
    "$@" && return 0
    [ "$n" = 3 ] || sleep 5
  done
  return 1
}

# An image left by an interrupted run is named after its process: discard it once
# that process is gone. A run still going in another worktree keeps its own.
for stale in "$tmp"/ddc-image-*.sparsebundle; do
  [ -e "$stale" ] || continue
  pid=${stale##*/ddc-image-}
  pid=${pid%%-*}
  kill -0 "$pid" 2>/dev/null && continue
  discard "$stale" || true
done

# The 1-minute load average before anything is built: the run's own work must not
# decide whether its CPU times are judged.
load_start=${LOAD_START:-$(sysctl -n vm.loadavg | awk '{ print $2 }')}
nonce=$(od -An -N4 -tx1 /dev/urandom | tr -d ' \n')
volume=ddc-load-$nonce
image=$tmp/ddc-image-$$-$nonce.sparsebundle
trap 'discard "$image" || true' EXIT

retry hdiutil create -type SPARSEBUNDLE -fs APFS -volname "$volume" -size "$cap" -quiet "${image%.sparsebundle}"
attach=$(retry hdiutil attach -nobrowse -owners on -plist "$image")
device= mount= i=0
while dev=$(plutil -extract "system-entities.$i.dev-entry" raw -o - - <<<"$attach" 2>/dev/null); do
  if at=$(plutil -extract "system-entities.$i.mount-point" raw -o - - <<<"$attach" 2>/dev/null); then
    device=$dev
    mount=$at
  fi
  i=$((i + 1))
done
if [ -z "$mount" ]; then
  echo "load-image: hdiutil attached $image but mounted no volume" >&2
  exit 1
fi

# Before the first tree: Spotlight and fseventsd leave the volume alone, and the
# marker tells the generator's guard this is its own image.
touch "$mount/.metadata_never_index"
mkdir -p "$mount/.fseventsd"
touch "$mount/.fseventsd/no_log"
printf '%s' "$nonce" >"$mount/.ddc-loadgen"

mkdir -p "$out"
field() { plutil -extract "$2" raw -o - "$1" 2>/dev/null || echo -; }
seconds() { awk -v ms="$(field "$1" "$2")" 'BEGIN { if (ms == "-") print "-"; else printf "%.1f", ms / 1000 }'; }
echo "image $image on $device at $mount; $size: $entries entries, seed $seed"
printf '%-14s %10s %9s %9s %11s %s\n' profile entries build_s scan_s differences failures
failed= tables=()
for p in $profiles; do
  mkdir "$mount/$p"
  json=$out/$size-$p.json
  if [ -n "${LOAD_CLI:-}" ]; then
    rm -f "$out/$size-$p.tsv"
    "$loadgen" run "$p" --entries "$entries" --seed "$seed" --into "$mount/$p" --device "$device" \
      --cli "$LOAD_CLI" --repeat "${LOAD_REPEAT:-3}" --size "$size" --date "${LOAD_DATE:--}" \
      --commit "${LOAD_COMMIT:--}" --out "$out" --mode "${LOAD_MODE:-record}" \
      --load-start "$load_start" >"$json" || failed="$failed $p"
    [ -e "$out/$size-$p.tsv" ] && tables+=("$out/$size-$p.tsv")
  elif [ -n "${LOAD_HEAP:-}" ]; then
    "$loadgen" heap "$p" --entries "$entries" --seed "$seed" --into "$mount/$p" --device "$device" \
      --cli "$LOAD_HEAP" --size "$size" --out "$out" >"$json" || failed="$failed $p"
  else
    "$loadgen" build "$p" --entries "$entries" --seed "$seed" --into "$mount/$p" --device "$device" \
      --verify --brief >"$json" || failed="$failed $p"
  fi
  printf '%-14s %10s %9s %9s %11s %s\n' "$p" "$(field "$json" entries)" "$(seconds "$json" build_ms)" \
    "$(seconds "$json" verify_ms)" "$(field "$json" difference_count)" "$(field "$json" failure_count)"
done
if [ ${#tables[@]} -gt 0 ]; then
  rm -f "$out/load-$size.tsv"
  "$loadgen" table "${tables[@]}" --into "$out/load-$size.tsv"
  echo "measurements: $out/load-$size.tsv"
fi
echo "image size on disk: $(du -sh "$image" | cut -f1); volume used: $(df -h "$mount" | awk 'NR == 2 { print $3 }')"
if [ -n "$failed" ]; then
  echo "load-image: failed:$failed. A number under differences is a mismatch with the manifest," \
    "one under failures a measured run that failed (both listed in $out/$size-<profile>.json);" \
    "a dash is a build that did not finish (its message is above)." >&2
  exit 1
fi
