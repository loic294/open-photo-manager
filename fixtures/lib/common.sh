# shellcheck shell=bash
# Helpers shared by generate.sh, verify.sh and mutations.sh. Sourced, never executed.
#
# Portability rules (the scripts also run in the Debian runtime image):
# bash 3.2 compatible (no associative arrays, mapfile or ${v,,}; guard empty arrays under set -u),
# and only coreutils, awk, find, xargs, od, tr, sort, exiftool, ffmpeg, vips and heif-enc.
# No jq, python, perl, xxd or mktemp. Scratch files live inside the output directory.

export LC_ALL=C

FX_LIB_DIR=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
FX_DIR=$(dirname "$FX_LIB_DIR")
FX_TAG=${FX_TAG:-fixtures}

fx_log() { printf '[%s] %s\n' "$FX_TAG" "$*"; }
fx_warn() { printf '[%s] warning: %s\n' "$FX_TAG" "$*" >&2; }
fx_die() {
  printf '[%s] error: %s\n' "$FX_TAG" "$*" >&2
  exit 1
}

# set -e alone ends the script without a word when a helper fails; this names the command that did. Failures inside
# subshells ($(...), pipelines) are left to the parent, which prints one line if the failure reaches it.
fx_on_err() {
  if [ "${BASH_SUBSHELL:-0}" != 0 ]; then return 0; fi
  printf '[%s] error: "%s" failed with status %s (%s line %s)\n' "$FX_TAG" "$2" "$1" "$3" "$4" >&2
}
set -E
trap 'fx_on_err $? "$BASH_COMMAND" "${BASH_SOURCE[0]##*/}" "$LINENO"' ERR

# fx_need CMD... : fail with install hints when a required tool is missing.
fx_need() {
  local missing='' c
  for c in "$@"; do
    if ! command -v "$c" >/dev/null 2>&1; then
      missing="$missing $c"
    fi
  done
  if [ -n "$missing" ]; then
    fx_die "missing required tools:$missing
  Debian/Ubuntu: apt-get install libimage-exiftool-perl ffmpeg libvips-tools libheif-examples
  macOS:         brew install exiftool ffmpeg vips libheif"
  fi
}

# Sets FX_SHA1 (command array) and FX_STAT_FMT (stat arguments printing "mtime<TAB>size<TAB>name").
fx_detect_portability() {
  if command -v sha1sum >/dev/null 2>&1; then
    FX_SHA1=(sha1sum)
  elif command -v shasum >/dev/null 2>&1; then
    FX_SHA1=(shasum -a 1)
  else
    fx_die "missing required tool: sha1sum or shasum"
  fi
  if stat --version >/dev/null 2>&1; then
    FX_STAT_FMT=(-c $'%Y\t%s\t%n')
  else
    FX_STAT_FMT=(-f $'%m\t%z\t%N')
  fi
}

fx_sha1_stdin() { "${FX_SHA1[@]}" | awk '{ print $1 }'; }

# fx_touch STAMP FILE... : STAMP is YYYYMMDDhhmm.SS, always interpreted as UTC.
fx_touch() {
  local stamp=$1
  shift
  TZ=UTC touch -t "$stamp" -- "$@"
}

# The converters below return their result in FX_R so hot loops do not fork.

# 2022-12-31T23:59:59 -> 202212312359.59
fx_stamp() {
  local s=${1//[-:T ]/}
  FX_R="${s:0:12}.${s:12:2}"
}

# 2022-12-31T23:59:59 -> 2022:12:31 23:59:59
fx_exif_dt() {
  FX_R="${1:0:4}:${1:5:2}:${1:8:2} ${1:11:8}"
}

# +02:00 / -05:30 -> signed minutes
fx_offmin() {
  local sign=1 hh mm
  if [ "${1:0:1}" = "-" ]; then sign=-1; fi
  hh=${1:1:2}
  mm=${1:4:2}
  FX_R=$((sign * (10#$hh * 60 + 10#$mm)))
}

# Epoch seconds -> 2024-01-01T00:00:00 (proleptic Gregorian, UTC).
fx_iso_from_epoch() {
  local e=$1 days sec z era doe yoe y doy mp d m
  days=$((e / 86400))
  sec=$((e % 86400))
  z=$((days + 719468))
  era=$((z / 146097))
  doe=$((z - era * 146097))
  yoe=$(((doe - doe / 1460 + doe / 36524 - doe / 146096) / 365))
  y=$((yoe + era * 400))
  doy=$((doe - (365 * yoe + yoe / 4 - yoe / 100)))
  mp=$(((5 * doy + 2) / 153))
  d=$((doy - (153 * mp + 2) / 5 + 1))
  if [ "$mp" -lt 10 ]; then m=$((mp + 3)); else m=$((mp - 9)); fi
  if [ "$m" -le 2 ]; then y=$((y + 1)); fi
  printf -v FX_R '%04d-%02d-%02dT%02d:%02d:%02d' "$y" "$m" "$d" $((sec / 3600)) $((sec % 3600 / 60)) $((sec % 60))
}

# fx_hex_fmt HEX : sets FX_R to a printf format holding one \xHH escape per byte of the hex string.
# Writing tiny files with the printf builtin avoids a process per file.
fx_hex_fmt() {
  local hex=$1 i out=''
  for ((i = 0; i < ${#hex}; i += 2)); do
    out="$out\\x${hex:i:2}"
  done
  FX_R=$out
}

# fx_printf_fmt FILE : prints a printf format that reproduces the file's bytes exactly (NULs included).
fx_printf_fmt() {
  fx_hex_fmt "$(od -An -v -tx1 "$1" | tr -d ' \n')"
  printf '%s' "$FX_R"
}

# fx_stamp_get FILE KEY : value of a KEY=value line in a stamp file.
fx_stamp_get() {
  awk -v k="$2" '{ i = index($0, "="); if (i > 0 && substr($0, 1, i - 1) == k) { print substr($0, i + 1); exit } }' "$1" 2>/dev/null || true
}

# Extension tables (mirrors crates/opm-core/src/consts.rs and kind.rs; keep in sync).
# fx_kind_of NAME : sets FX_KIND (image|raw|video|sidecar|other) and FX_DBK (Image|Video|Sidecar|Other).
# The extension is whatever follows the last dot, except for ".hidden" style names (opm_core::names::split_name).
fx_kind_of() {
  local name=$1 rest ext='' nc=0
  rest=${name#?}
  case $rest in
    *.*) ext=${name##*.} ;;
  esac
  if shopt -q nocasematch; then nc=1; fi
  shopt -s nocasematch
  case $ext in
    jpg | jpeg | png | gif | webp | tif | tiff | bmp | heic | heif | avif | jxl)
      FX_KIND=image
      FX_DBK=Image
      ;;
    cr2 | cr3 | crw | nef | nrw | arw | srf | sr2 | dng | raf | orf | rw2 | pef | srw | x3f | 3fr | erf | mrw | dcr | kdc | raw)
      FX_KIND=raw
      FX_DBK=Image
      ;;
    mp4 | m4v | mov | avi | mkv | webm | mts | m2ts | mpg | mpeg | wmv | 3gp | mxf)
      FX_KIND=video
      FX_DBK=Video
      ;;
    xmp | json | aae | dop | pp3 | xml)
      FX_KIND=sidecar
      FX_DBK=Sidecar
      ;;
    *)
      FX_KIND=other
      FX_DBK=Other
      ;;
  esac
  if [ "$nc" = 0 ]; then shopt -u nocasematch; fi
}

# fx_camera MAKE MODEL : the camera label the indexer stores. A model that already starts with the make
# (case-insensitive) is used as is, otherwise "Make Model".
fx_camera() {
  local make=$1 model=$2 nc=0
  if [ -z "$model" ]; then
    FX_R=$make
    return 0
  fi
  if [ -z "$make" ]; then
    FX_R=$model
    return 0
  fi
  if shopt -q nocasematch; then nc=1; fi
  shopt -s nocasematch
  case $model in
    "$make"*) FX_R=$model ;;
    *) FX_R="$make $model" ;;
  esac
  if [ "$nc" = 0 ]; then shopt -u nocasematch; fi
}

# fx_tags KEYWORDS HIERARCHY : sorted unique keyword list. KEYWORDS is comma separated (dc:subject),
# HIERARCHY is a "A|B|C" path (lr:hierarchicalSubject) that contributes every component.
fx_tags() {
  local kw=${1//,/$'\n'} hier=${2//|/$'\n'}
  FX_R=$(printf '%s\n%s\n' "$kw" "$hier" | awk 'NF' | sort -u | paste -sd, -)
}

# fx_write_xmp FILE [key=value ...] : writes an XMP sidecar and sets X_RATING (0-5), X_FLAG, X_TAGS, X_CAMERA, X_DATE
# to what the indexer should extract from it, plus X_HAS_<FIELD>=1 for the fields the sidecar really carries.
# Keys: rating pick make model date kw hier style (attr|elem).
# A rating of -1 means "rejected" (stored as rating 0 with the rejected flag).
fx_write_xmp() {
  local file=$1 kv k v
  shift
  local rating='' pick='' make='' model='' date='' kw='' hier='' style=attr
  for kv in "$@"; do
    k=${kv%%=*}
    v=${kv#*=}
    case $k in
      rating) rating=$v ;;
      pick) pick=$v ;;
      make) make=$v ;;
      model) model=$v ;;
      date) date=$v ;;
      kw) kw=$v ;;
      hier) hier=$v ;;
      style) style=$v ;;
      *) fx_die "fx_write_xmp: unknown option '$k'" ;;
    esac
  done
  X_RATING=0
  X_FLAG=none
  X_HAS_RATING=0
  X_HAS_FLAG=0
  if [ -n "$rating" ]; then
    X_HAS_RATING=1
    if [ "$rating" -lt 0 ]; then
      X_RATING=0
      X_FLAG=rejected
      X_HAS_FLAG=1
    else
      X_RATING=$rating
    fi
  fi
  case $pick in
    1)
      X_FLAG=selected
      X_HAS_FLAG=1
      ;;
    -1)
      X_FLAG=rejected
      X_HAS_FLAG=1
      ;;
  esac
  fx_camera "$make" "$model"
  X_CAMERA=$FX_R
  X_HAS_CAMERA=0
  if [ -n "$make$model" ]; then X_HAS_CAMERA=1; fi
  fx_tags "$kw" "$hier"
  X_TAGS=$FX_R
  X_HAS_TAGS=0
  if [ -n "$X_TAGS" ]; then X_HAS_TAGS=1; fi
  X_DATE=$date
  X_HAS_DATE=0
  if [ -n "$date" ]; then X_HAS_DATE=1; fi

  local attrs='' body='' item IFS_SAVE=$IFS
  if [ "$style" = elem ]; then
    if [ -n "$rating" ]; then body="$body   <xmp:Rating>$rating</xmp:Rating>"$'\n'; fi
    if [ -n "$pick" ]; then body="$body   <xmpDM:pick>$pick</xmpDM:pick>"$'\n'; fi
    if [ -n "$make" ]; then body="$body   <tiff:Make>$make</tiff:Make>"$'\n'; fi
    if [ -n "$model" ]; then body="$body   <tiff:Model>$model</tiff:Model>"$'\n'; fi
    if [ -n "$date" ]; then body="$body   <exif:DateTimeOriginal>$date</exif:DateTimeOriginal>"$'\n'; fi
  else
    if [ -n "$rating" ]; then attrs="$attrs"$'\n'"    xmp:Rating=\"$rating\""; fi
    if [ -n "$pick" ]; then attrs="$attrs"$'\n'"    xmpDM:pick=\"$pick\""; fi
    if [ -n "$make" ]; then attrs="$attrs"$'\n'"    tiff:Make=\"$make\""; fi
    if [ -n "$model" ]; then attrs="$attrs"$'\n'"    tiff:Model=\"$model\""; fi
    if [ -n "$date" ]; then attrs="$attrs"$'\n'"    exif:DateTimeOriginal=\"$date\""; fi
  fi
  if [ -n "$kw" ]; then
    body="$body   <dc:subject>"$'\n'"    <rdf:Bag>"$'\n'
    IFS=,
    for item in $kw; do
      body="$body     <rdf:li>$item</rdf:li>"$'\n'
    done
    IFS=$IFS_SAVE
    body="$body    </rdf:Bag>"$'\n'"   </dc:subject>"$'\n'
  fi
  if [ -n "$hier" ]; then
    body="$body   <lr:hierarchicalSubject>"$'\n'"    <rdf:Bag>"$'\n'"     <rdf:li>$hier</rdf:li>"$'\n'"    </rdf:Bag>"$'\n'"   </lr:hierarchicalSubject>"$'\n'
  fi
  {
    printf '%s\n' '<?xpacket begin="" id="W5M0MpCehiHzreSzNTczkc9d"?>'
    printf '%s\n' '<x:xmpmeta xmlns:x="adobe:ns:meta/" x:xmptk="opm-fixtures">'
    printf '%s\n' ' <rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#">'
    printf '%s' '  <rdf:Description rdf:about=""'
    printf '%s\n' ''
    printf '%s\n' '    xmlns:xmp="http://ns.adobe.com/xap/1.0/"'
    printf '%s\n' '    xmlns:xmpDM="http://ns.adobe.com/xmp/1.0/DynamicMedia/"'
    printf '%s\n' '    xmlns:dc="http://purl.org/dc/elements/1.1/"'
    printf '%s\n' '    xmlns:lr="http://ns.adobe.com/lightroom/1.0/"'
    printf '%s\n' '    xmlns:tiff="http://ns.adobe.com/tiff/1.0/"'
    printf '%s' '    xmlns:exif="http://ns.adobe.com/exif/1.0/"'
    if [ -n "$attrs" ]; then printf '%s' "$attrs"; fi
    printf '%s\n' '>'
    if [ -n "$body" ]; then printf '%s' "$body"; fi
    printf '%s\n' '  </rdf:Description>'
    printf '%s\n' ' </rdf:RDF>'
    printf '%s\n' '</x:xmpmeta>'
    printf '%s\n' '<?xpacket end="w"?>'
  } >"$file"
}

# fx_tree_fingerprint OUT : hash of the generated tree (names, sizes, mtimes, link targets).
# It never fails: unreadable directories (chmod 000 scenarios) are simply not listed.
fx_tree_fingerprint() {
  local out=$1
  {
    (cd "$out" && find volumes outside -type d -print 2>/dev/null) | sort || true
    (cd "$out" && find volumes outside -type f -exec stat "${FX_STAT_FMT[@]}" {} + 2>/dev/null) | sort || true
    (cd "$out" && find volumes outside -type l -print 2>/dev/null) | sort || true
    (cd "$out" && find volumes outside -type l -exec readlink {} \; 2>/dev/null) | sort || true
  } | fx_sha1_stdin
}

# fx_collect_meta ROOT STAT_OUT SHA_OUT DIR... : size, mtime and sha1 of every regular file below ROOT/volumes/DIR.
# STAT_OUT lines are mtime<TAB>size<TAB>volume/path and SHA_OUT lines are sha1<2 spaces>volume/path.
fx_collect_meta() {
  local root=$1 statf=$2 shaf=$3
  shift 3
  (cd "$root/volumes" && find "$@" -type f -exec stat "${FX_STAT_FMT[@]}" {} +) >"$statf"
  (cd "$root/volumes" && find "$@" -type f -print0 | xargs -0 -r "${FX_SHA1[@]}") >"$shaf"
}

# fx_restore_perms OUT : undo the chmod 000 scenarios so the tree can be listed, scanned or removed.
fx_restore_perms() {
  local out=$1 f
  if [ -f "$out/.fixtures-unreadable" ]; then
    while IFS= read -r f; do
      if [ -e "$out/$f" ]; then
        chmod u+rwx "$out/$f" 2>/dev/null || true
        chmod -R u+rwX "$out/$f" 2>/dev/null || true
      fi
    done <"$out/.fixtures-unreadable"
  fi
  chmod -R u+rwX "$out" 2>/dev/null || true
}

# fx_clean_out OUT : remove a generated tree. Refuses directories this tool did not create.
fx_clean_out() {
  local out=$1
  if [ ! -e "$out" ] && [ ! -L "$out" ]; then return 0; fi
  if [ ! -d "$out" ] || [ -L "$out" ]; then
    fx_die "refusing to remove '$out': not a plain directory"
  fi
  if [ -n "$(ls -A "$out" 2>/dev/null)" ] && [ ! -f "$out/.fixtures-owned" ]; then
    fx_die "refusing to remove '$out': it was not created by fixtures/generate.sh (no .fixtures-owned marker)"
  fi
  fx_restore_perms "$out"
  rm -rf "$out"
}

# fx_abs_path PATH : absolute path with the existing parent resolved; PATH itself need not exist.
fx_abs_path() {
  local p=$1 parent base
  while [ "${#p}" -gt 1 ] && [ "${p%/}" != "$p" ]; do p=${p%/}; done
  case $p in
    /*) ;;
    *) p="$PWD/$p" ;;
  esac
  if [ "$p" = "/" ]; then
    FX_R=/
    return 0
  fi
  base=${p##*/}
  parent=${p%/*}
  if [ -z "$parent" ]; then parent=/; fi
  if [ -d "$parent" ]; then
    parent=$(cd "$parent" && pwd -P)
    FX_R="${parent%/}/$base"
  else
    FX_R=$p
  fi
}

# fx_safe_out OUT : refuse output directories that would be catastrophic to clean.
fx_safe_out() {
  local out=$1 root
  root=$(cd "$FX_DIR/.." && pwd -P)
  if [ "$out" = "/" ] || [ "$out" = "${HOME:-/}" ] || [ "$out" = "$root" ] || [ "$out" = "$FX_DIR" ]; then
    fx_die "refusing to use '$out' as the output directory"
  fi
}

# fx_nproc : number of CPUs (1 when unknown) in FX_R.
fx_nproc() {
  local n
  n=$(getconf _NPROCESSORS_ONLN 2>/dev/null || nproc 2>/dev/null || echo 1)
  case $n in
    '' | *[!0-9]*) n=1 ;;
  esac
  FX_R=$n
}

# fx_rate COUNT SECONDS UNIT : human readable throughput in FX_R, e.g. "646 files in 9s (71 files/s)".
fx_rate() {
  if [ "$2" -le 0 ]; then
    FX_R="$1 $3 in <1s"
  else
    FX_R="$1 $3 in ${2}s ($(($1 / $2)) $3/s)"
  fi
}
