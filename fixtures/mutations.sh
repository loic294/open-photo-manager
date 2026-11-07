#!/usr/bin/env bash
# Applies a deterministic set of changes to a generated small-scale fixture tree so incremental-scan and watcher
# tests can exercise stale-index handling. Everything happens below volumes/photos/stale/ (scenarios stale.*).
#
#   fixtures/mutations.sh [--out DIR] [--list]
#
#   rename         stale/rename_me.jpg      becomes stale/renamed.jpg (same bytes)
#   change_bytes   stale/bytes_changed.jpg  one byte of the JPEG header flipped; size and mtime stay the same
#   touch          stale/mtime_only.jpg     new mtime, same bytes
#   delete_dir     stale/doomed_dir         folder with its three photos removed
#   add_sidecar    stale/has_new_sidecar.xmp  new XMP (rating 4) next to has_new_sidecar.jpg
#   edit_sidecar   stale/touched.xmp        rating 2 becomes 3 (same size) and gets a new mtime
#   add_file       stale/added_new.jpg      new photo: mtime_only.jpg with one byte of the JPEG header flipped
#
# New and touched files get the mtime 2024-06-15T12:00:00Z. The outcome (new size, sha1 and mtime of every touched
# file) is written to DIR/mutations.tsv. manifest.json keeps describing the baseline; fixtures/generate.sh --clean
# (or make fixtures) resets the tree. Applying the changes twice is refused.
set -euo pipefail

FX_TAG=mutations
# shellcheck source=lib/common.sh
. "$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/lib/common.sh"

MUT_STAMP=202406151200.00

usage() {
  printf '%s\n' \
    "Usage: fixtures/mutations.sh [--out DIR] [--list]" \
    "" \
    "  --out DIR   small-scale fixture tree to change (default: fixtures/out)" \
    "  --list      print the changes without applying them" \
    "  -h, --help  this text"
}

list_plan() {
  printf '%s\n' \
    "rename        photos/stale/rename_me.jpg -> photos/stale/renamed.jpg (same bytes)" \
    "change_bytes  photos/stale/bytes_changed.jpg: one byte of the JPEG header flipped, size and mtime unchanged" \
    "touch         photos/stale/mtime_only.jpg: mtime becomes 2024-06-15T12:00:00Z, bytes unchanged" \
    "delete_dir    photos/stale/doomed_dir: folder with 3 photos removed" \
    "add_sidecar   photos/stale/has_new_sidecar.xmp: new sidecar with rating 4 for has_new_sidecar.jpg" \
    "edit_sidecar  photos/stale/touched.xmp: rating 2 becomes 3, same size, mtime 2024-06-15T12:00:00Z" \
    "add_file      photos/stale/added_new.jpg: new photo (mtime_only.jpg with one byte of the JPEG header flipped)"
}

OUT_ARG='' LIST=0
while [ $# -gt 0 ]; do
  case $1 in
    --out)
      if [ $# -lt 2 ]; then fx_die "--out needs a value"; fi
      OUT_ARG=$2
      shift 2
      ;;
    --out=*) OUT_ARG=${1#--out=} && shift ;;
    --list) LIST=1 && shift ;;
    -h | --help)
      usage
      exit 0
      ;;
    *) fx_die "unknown argument '$1' (try --help)" ;;
  esac
done
if [ "$LIST" = 1 ]; then
  list_plan
  exit 0
fi

fx_detect_portability
fx_need awk od dd stat touch cp cat
fx_abs_path "${OUT_ARG:-$FX_DIR/out}"
OUT=$FX_R
S="$OUT/volumes/photos/stale"
if [ ! -f "$OUT/.fixtures-stamp" ]; then
  fx_die "$OUT is not a generated fixture tree; run fixtures/generate.sh first"
fi
if [ -d "$OUT.lock" ]; then
  fx_die "generate.sh is running on $OUT (lock $OUT.lock)"
fi
if [ -e "$OUT/.mutations-applied" ]; then
  fx_die "the changes were already applied to $OUT; run fixtures/generate.sh --clean to reset it"
fi
if [ ! -d "$S" ] || [ ! -f "$S/rename_me.jpg" ] || [ ! -f "$S/touched.xmp" ]; then
  fx_die "$S is missing: only the small scale has the stale.* scenarios (fixtures/generate.sh --scale small)"
fi

REF="$OUT/.mutations-ref"
TMP="$OUT/.mutations-tmp"
ROWS="$OUT/.mutations-rows"
trap 'rm -f "$REF" "$TMP" "$ROWS"' EXIT
rm -f "$REF" "$TMP"
: >"$ROWS"
# The marker goes first: a run that fails half way must still leave a tree that generate.sh knows to rebuild.
printf 'applied by fixtures/mutations.sh; run fixtures/generate.sh --clean to reset\n' >"$OUT/.mutations-applied"

# record OP PATH NEWPATH FILE NOTE : one mutations.tsv row; the size, sha1 and mtime come from FILE ("-" when removed).
record() {
  local op=$1 path=$2 newpath=$3 file=$4 note=$5 line mtime size sha=-
  if [ "$file" = - ]; then
    mtime=- size=-
  else
    line=$(stat "${FX_STAT_FMT[@]}" "$file")
    mtime=${line%%$'\t'*}
    line=${line#*$'\t'}
    size=${line%%$'\t'*}
    sha=$(fx_sha1_stdin <"$file")
  fi
  printf '%s\tphotos\t%s\t%s\t%s\t%s\t%s\t%s\n' "$op" "$path" "$newpath" "$size" "$sha" "$mtime" "$note" >>"$ROWS"
}

# flip_byte FILE : XOR 1 into the last entry of the first quantization table, a byte of the JPEG header. The file stays
# a valid JPEG with the same size, EXIF and (practically) the same pixels, since that high-frequency step is applied to
# coefficients that are zero; flipping entropy-coded bytes instead would risk decoder errors. Entries 0 and 1 are never
# chosen so the table can not end up holding a zero.
flip_byte() {
  local f=$1 off cur
  off=$(od -An -v -tx1 -N 2048 "$f" | awk '
    function hx(s) { return (index("0123456789abcdef", substr(s, 1, 1)) - 1) * 16 + index("0123456789abcdef", substr(s, 2, 1)) - 1 }
    { for (i = 1; i <= NF; i++) b[n++] = $i }
    END {
      pos = 2
      while (pos + 4 < n && b[pos] == "ff" && b[pos + 1] != "da") {
        if (b[pos + 1] == "db" && b[pos + 4] == "00") {
          for (p = pos + 68; p > pos + 4; p--)
            if (b[p] != "00" && b[p] != "01") { print p; exit }
          exit
        }
        pos += 2 + hx(b[pos + 2]) * 256 + hx(b[pos + 3])
      }
    }')
  if [ -z "$off" ]; then fx_die "unexpected JPEG layout in $f"; fi
  cur=$(od -An -v -tx1 -j "$off" -N 1 "$f" | tr -d ' \n')
  # shellcheck disable=SC2059
  printf "\\$(printf '%03o' $((0x$cur ^ 1)))" | dd of="$f" bs=1 seek="$off" conv=notrunc 2>/dev/null
}

fx_log "changing $S"

mv "$S/rename_me.jpg" "$S/renamed.jpg"
record rename stale/rename_me.jpg stale/renamed.jpg "$S/renamed.jpg" "same bytes and mtime under a new name"

cp -p "$S/bytes_changed.jpg" "$REF"
flip_byte "$S/bytes_changed.jpg"
touch -r "$REF" "$S/bytes_changed.jpg"
record change_bytes stale/bytes_changed.jpg - "$S/bytes_changed.jpg" "one byte changed; size and mtime as before"

fx_touch "$MUT_STAMP" "$S/mtime_only.jpg"
record touch stale/mtime_only.jpg - "$S/mtime_only.jpg" "new mtime; sha1 unchanged"

rm -rf "$S/doomed_dir"
record delete_dir stale/doomed_dir - - "folder with gone_1.jpg, gone_2.jpg and sub/gone_3.jpg removed"

fx_write_xmp "$S/has_new_sidecar.xmp" rating=4
fx_touch "$MUT_STAMP" "$S/has_new_sidecar.xmp"
record add_sidecar stale/has_new_sidecar.xmp - "$S/has_new_sidecar.xmp" "new sidecar linked to has_new_sidecar.jpg by stem; rating 4"

awk '{ sub(/xmp:Rating="2"/, "xmp:Rating=\"3\"") } 1' "$S/touched.xmp" >"$TMP"
cat "$TMP" >"$S/touched.xmp"
fx_touch "$MUT_STAMP" "$S/touched.xmp"
record edit_sidecar stale/touched.xmp - "$S/touched.xmp" "rating 2 becomes 3; same size, new mtime"

cp "$S/mtime_only.jpg" "$S/added_new.jpg"
flip_byte "$S/added_new.jpg"
fx_touch "$MUT_STAMP" "$S/added_new.jpg"
record add_file stale/added_new.jpg - "$S/added_new.jpg" "new photo; EXIF of mtime_only.jpg, different bytes"

{
  printf '%s\n' '#op	volume	path	new_path	size	sha1	mtime	note'
  cat "$ROWS"
} >"$OUT/mutations.tsv"
fx_log "applied 7 changes; see $OUT/mutations.tsv"
