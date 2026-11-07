#!/usr/bin/env bash
# Checks a generated fixture tree against its manifest.
#
#   fixtures/verify.sh [--out DIR] [--full | --sample N] [--no-regen]
#
#  1. every file: size and mtime match manifest.tsv; no file is missing or unexpected (the folders in
#     manifest.dirs.tsv exist, symlinks point where the manifest says, chmod 000 entries are locked)
#  2. sha1: every file at small scale, a deterministic sample otherwise (--full forces all, --sample N sets the size)
#  3. exiftool re-reads the capture date, offset and camera of the files whose expected capture source is EXIF, and
#     reads nothing from the deliberately broken files (corrupt header, truncated, zero byte)
#  4. unless --no-regen: the generator runs again into DIR-verify and must produce the same sha1 set and manifest.json
# Exit status 0 means everything matched. Nothing is written outside DIR and DIR-verify.
set -euo pipefail

FX_TAG=verify
# shellcheck source=lib/common.sh
. "$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/lib/common.sh"

usage() {
  printf '%s\n' \
    "Usage: fixtures/verify.sh [--out DIR] [--full | --sample N] [--no-regen]" \
    "" \
    "  --out DIR     fixture tree to check (default: fixtures/out)" \
    "  --full        hash and exiftool-check every file (default: everything up to 25000 files, else a sample)" \
    "  --sample N    sample size for the hash and exiftool checks of big trees (default 5000)" \
    "  --no-regen    skip the determinism check (regenerating takes as long as generating)" \
    "  -h, --help    this text"
}

OUT_ARG='' SAMPLE=5000 FULL=0 REGEN=1
while [ $# -gt 0 ]; do
  case $1 in
    --out | --sample)
      if [ $# -lt 2 ]; then fx_die "$1 needs a value"; fi
      if [ "$1" = --out ]; then OUT_ARG=$2; else SAMPLE=$2; fi
      shift 2
      ;;
    --out=*) OUT_ARG=${1#--out=} && shift ;;
    --sample=*) SAMPLE=${1#--sample=} && shift ;;
    --full) FULL=1 && shift ;;
    --no-regen) REGEN=0 && shift ;;
    -h | --help)
      usage
      exit 0
      ;;
    *) fx_die "unknown argument '$1' (try --help)" ;;
  esac
done
case $SAMPLE in
  '' | *[!0-9]* | 0) fx_die "--sample needs a positive number" ;;
esac

fx_detect_portability
fx_need exiftool awk find xargs sort stat cmp diff cut readlink
fx_abs_path "${OUT_ARG:-$FX_DIR/out}"
OUT=$FX_R
MAN="$OUT/manifest.tsv"
if [ ! -f "$OUT/.fixtures-stamp" ] || [ ! -f "$MAN" ]; then
  fx_die "$OUT is not a generated fixture tree; run fixtures/generate.sh first"
fi
if [ -e "$OUT/.mutations-applied" ]; then
  fx_die "mutations.sh changed $OUT on purpose; run fixtures/generate.sh to reset it"
fi
SCALE=$(fx_stamp_get "$OUT/.fixtures-stamp" scale)
FILES=$(fx_stamp_get "$OUT/.fixtures-stamp" files)
V=$OUT/.verify
FAILS=0
IS_ROOT=0
if [ "$(id -u)" = 0 ]; then IS_ROOT=1; fi

on_exit() {
  local rc=$?
  rm -rf "$V"
  return "$rc"
}
trap on_exit EXIT
trap 'exit 130' INT TERM
rm -rf "$V"
mkdir -p "$V"

# report LABEL : reads "FAIL: ..." lines on stdin, prints the first few and counts them in FAILS.
report() {
  local n=0 line
  while IFS= read -r line; do
    n=$((n + 1))
    if [ "$n" -le 15 ]; then printf '  %s\n' "$line" >&2; fi
  done
  if [ "$n" -gt 15 ]; then printf '  ... and %d more\n' $((n - 15)) >&2; fi
  FAILS=$((FAILS + n))
  if [ "$n" = 0 ]; then fx_log "ok: $1"; else fx_warn "$1: $n problem(s)"; fi
}

NFILES=$(awk -F '\t' 'substr($0, 1, 1) != "#" && $3 == "file" { n++ } END { print n + 0 }' "$MAN")
STEP=1
if [ "$FULL" = 0 ] && [ "$NFILES" -gt 25000 ]; then
  STEP=$((NFILES / SAMPLE))
  if [ "$STEP" -lt 1 ]; then STEP=1; fi
fi
fx_log "checking $OUT ($SCALE scale, $NFILES files, hashing every $STEP-th file)"

# sample_list KIND : paths (volume/path) of the files to hash (KIND=all) or to read with exiftool (KIND=exif).
sample_list() {
  awk -F '\t' -v step="$STEP" -v kind="$1" '
    FILENAME == DUPS {
      if (substr($0, 1, 1) == "#" || $1 != "1" || kind != "all") next
      if (!($2 in G)) G[$2] = ++ng
      if (G[$2] % 40 == 0) print $3 "/" $4
      next
    }
    substr($0, 1, 1) == "#" || $3 != "file" || $18 == "unreadable_dir" { next }
    kind == "exif" && !($12 == "exif" && $10 == "1" && ($7 == "image" || $7 == "raw")) { next }
    { n++; if (step == 1 || n % step == 0 || $9 !~ /^bulk\./) print $1 "/" $2 }
  ' DUPS="$OUT/manifest.dups.tsv" "$MAN" "$OUT/manifest.dups.tsv" | sort -u
}

# ---------------------------------------------------------------- 1 and 2: size, mtime, sha1
(cd "$OUT/volumes" && find photos archive -type f -exec stat "${FX_STAT_FMT[@]}" {} + 2>/dev/null) >"$V/stat.tsv" || true
sample_list all >"$V/hash.list"
(cd "$OUT/volumes" && tr '\n' '\0' <"$V/hash.list" | xargs -0 -r "${FX_SHA1[@]}" 2>/dev/null) >"$V/sha.tsv" || true

report "size, mtime and sha1 ($(awk 'END { print NR }' "$V/sha.tsv") hashed)" < <(
  awk -F '\t' -v MAN="$MAN" -v STATF="$V/stat.tsv" -v SHAF="$V/sha.tsv" '
    function bad(m) { print "FAIL: " m }
    FILENAME == MAN {
      if (substr($0, 1, 1) == "#" || $3 != "file") next
      k = $1 "/" $2
      SZ[k] = $4; SHA[k] = $5; MT[k] = $6; UNR[k] = ($18 == "unreadable_dir")
      next
    }
    FILENAME == STATF {
      if (!($3 in SZ)) { bad("unexpected file " $3); next }
      SEEN[$3] = 1
      if (UNR[$3]) next
      if ($2 != SZ[$3]) bad("size of " $3 " is " $2 ", the manifest says " SZ[$3])
      if ($1 != MT[$3]) bad("mtime of " $3 " is " $1 ", the manifest says " MT[$3])
      next
    }
    FILENAME == SHAF {
      k = substr($0, 43)
      if (!(k in SHA)) { bad("hashed an unknown file " k); next }
      if (substr($0, 1, 40) != SHA[k]) bad("sha1 of " k " is " substr($0, 1, 40) ", the manifest says " SHA[k])
      next
    }
    END { for (k in SZ) if (!(k in SEEN) && !UNR[k]) bad("missing file " k) }
  ' "$MAN" "$V/stat.tsv" "$V/sha.tsv"
)

# ---------------------------------------------------------------- symlinks, folders, locked entries
check_links_and_dirs() {
  local vol rel state scn key target f
  while IFS=$'\t' read -r key target; do
    if [ ! -L "$OUT/volumes/$key" ]; then
      echo "FAIL: $key is not a symlink"
    elif [ "$(readlink "$OUT/volumes/$key")" != "$target" ]; then
      echo "FAIL: $key points to $(readlink "$OUT/volumes/$key"), the manifest says $target"
    fi
  done < <(awk -F '\t' 'substr($0, 1, 1) != "#" && $3 == "symlink" { print $1 "/" $2 "\t" $19 }' "$MAN")
  while IFS=$'\t' read -r vol rel state scn; do
    if [ ! -d "$OUT/volumes/$vol/$rel" ]; then echo "FAIL: folder $vol/$rel is missing"; fi
  done < <(awk 'substr($0, 1, 1) != "#"' "$OUT/manifest.dirs.tsv")
  if [ "$IS_ROOT" = 0 ] && [ -s "$OUT/.fixtures-unreadable" ]; then
    while IFS= read -r f; do
      if [ -r "$OUT/$f" ]; then echo "FAIL: $f should be unreadable (chmod 000)"; fi
    done <"$OUT/.fixtures-unreadable"
  fi
}
report "symlinks, folders and locked entries" < <(check_links_and_dirs)

# ---------------------------------------------------------------- 3: exiftool re-read
sample_list exif >"$V/exif.list"
(cd "$OUT/volumes" && tr '\n' '\0' <"$V/exif.list" |
  xargs -0 -r exiftool -charset filename=utf8 -T -Directory -FileName -DateTimeOriginal -CreateDate -Make -Model -OffsetTimeOriginal 2>/dev/null) >"$V/exif.tsv" || true
report "exiftool re-read ($(awk 'END { print NR }' "$V/exif.list") files)" < <(
  awk -F '\t' -v MAN="$MAN" -v LIST="$V/exif.list" '
    function bad(m) { print "FAIL: " m }
    function exif_of(iso,   d) { d = substr(iso, 1, 10); gsub("-", ":", d); return d " " substr(iso, 12, 8) }
    function cam(make, model) {
      if (make == "-" && model == "-") return "-"
      if (make == "-") return model
      if (model == "-") return make
      if (tolower(substr(model, 1, length(make))) == tolower(make)) return model
      return make " " model
    }
    function minutes(s,   sg) {
      if (s == "-") return "-"
      sg = (substr(s, 1, 1) == "-") ? -1 : 1
      return sg * (substr(s, 2, 2) * 60 + substr(s, 5, 2))
    }
    FILENAME == MAN {
      if (substr($0, 1, 1) == "#") next
      k = $1 "/" $2
      CAP[k] = $11; OFF[k] = $13; CAMX[k] = $14; SC[k] = $9
      next
    }
    FILENAME == LIST { REQ[$0] = 1; next }
    {
      k = $1 "/" $2
      if (!(k in REQ)) next
      SEEN[k] = 1
      got = ($3 != "-") ? $3 : $4
      if (got != exif_of(CAP[k])) bad(k ": exiftool reads capture " got ", the manifest says " exif_of(CAP[k]))
      if (SC[k] !~ /^sidecar[.]/ && cam($5, $6) != CAMX[k]) bad(k ": exiftool reads camera [" cam($5, $6) "], the manifest says [" CAMX[k] "]")
      if (OFF[k] != "-" && minutes($7) != OFF[k]) bad(k ": exiftool reads offset " $7 ", the manifest says " OFF[k] " minutes")
    }
    END { for (k in REQ) if (!(k in SEEN)) bad("exiftool returned nothing for " k) }
  ' "$MAN" "$V/exif.list" "$V/exif.tsv"
)

# ---------------------------------------------------------------- 3b: broken files stay unreadable
check_broken() {
  local key got
  while IFS= read -r key; do
    got=$(cd "$OUT/volumes" && exiftool -s3 -DateTimeOriginal -CreateDate -Make "$key" 2>/dev/null) || true
    if [ -n "$got" ]; then echo "FAIL: $key must have unreadable metadata but exiftool reads [$got]"; fi
  done < <(awk -F '\t' 'substr($0, 1, 1) != "#" && $9 ~ /^exif[.](corrupt_header|truncated|zero_byte)$/ { print $1 "/" $2 }' "$MAN")
}
report "broken metadata stays unreadable" < <(check_broken)

# ---------------------------------------------------------------- 4: determinism
regen_compare() {
  local vout="$OUT-verify" args n
  args=(--scale "$SCALE")
  if [ "$SCALE" = large ]; then args=("${args[@]}" --files "$FILES"); fi
  fx_log "regenerating into $vout to compare" >&2
  if ! "$FX_DIR/generate.sh" --out "$vout" --clean "${args[@]}" >"$V/regen.log" 2>&1; then
    echo "FAIL: regenerating failed (last lines of the log follow)"
    awk '{ l[NR] = $0 } END { for (i = (NR > 8 ? NR - 7 : 1); i <= NR; i++) print "FAIL:   " l[i] }' "$V/regen.log"
    return 0
  fi
  n=$(diff <(awk -F '\t' 'substr($0, 1, 1) != "#" && $3 != "symlink" { print $1 "\t" $2 "\t" $4 "\t" $5 "\t" $6 }' "$MAN" | sort) \
    <(awk -F '\t' 'substr($0, 1, 1) != "#" && $3 != "symlink" { print $1 "\t" $2 "\t" $4 "\t" $5 "\t" $6 }' "$vout/manifest.tsv" | sort) | awk 'END { print NR }') || true
  if [ "$n" != 0 ]; then echo "FAIL: the regenerated tree differs from this one in $n line(s) of (volume, path, size, sha1, mtime)"; fi
  if ! cmp -s "$OUT/manifest.json" "$vout/manifest.json"; then echo "FAIL: the regenerated manifest.json differs"; fi
  "$FX_DIR/generate.sh" --out "$vout" --clean-only >/dev/null 2>&1 || true
}
if [ "$REGEN" = 1 ]; then
  report "regenerated tree is identical" < <(regen_compare)
fi

if [ "$FAILS" != 0 ]; then
  fx_die "$FAILS problem(s) found; fixtures/generate.sh --clean rebuilds the tree"
fi
fx_log "all checks passed"
