# shellcheck shell=bash
# Registry and file-creation helpers for the fixture catalogue. Sourced by generate.sh after common.sh.
#
# The caller provides OUT (absolute output dir), WORK ($OUT/.work) and STAGE ($WORK/stage).
#
# Every path created under $OUT/volumes is recorded in $WORK/reg.tsv, one row per path with 23 tab separated columns
# ("-" = null):
#   1 volume   2 path   3 type (file|symlink)   4 kind   5 db_kind   6 scenario
#   7 capture (naive ISO)   8 capture source   9 offset minutes   10 tz dependent (0|1)   11 camera
#   12 rating   13 flag   14 tags (csv)   15 indexable (0|1)   16 note   17 unreadable (file|dir)
#   18 link target   19 width   20 height   21 duration ms   22 location (lat,lon)   23 skip reason
# manifest.awk joins the registry with the size, mtime and sha1 measured on the finished tree, and cross-checks it.

DEFAULT_MTIME=2020-01-01T12:00:00
TPL_SENTINEL='2000:01:01 00:00:00'
TPL_CAMS=("Canon|Canon EOS R5" "NIKON|D850" "Apple|iPhone 15 Pro" "Sony|ILCE-7M4")
TPL_PRE=()
TPL_SUF=()

media_init() {
  SEED=0
  STAGE_N=0
  SCN_KEY=''
  FX_KEY=''
  STAGE_OK=1
  STAGE_REASON=''
  X_RATING=0 X_FLAG=none X_TAGS='' X_CAMERA='' X_DATE=''
  X_HAS_RATING=0 X_HAS_FLAG=0 X_HAS_TAGS=0 X_HAS_CAMERA=0 X_HAS_DATE=0
  mkdir -p "$STAGE" "$WORK/probe"
  local f
  for f in reg links dupgroups dups dirscn skipped scenarios touch copies; do
    : >"$WORK/$f.tsv"
  done
  : >"$WORK/exiftool.args"
  : >"$WORK/unreadable.list"
}

# ---------------------------------------------------------------- options

_reset_opts() {
  o_fmt= o_size= o_seed= o_noise= o_tag= o_opt=
  o_dt= o_cd= o_sub= o_offset= o_make= o_model= o_rating= o_pick= o_kw= o_hier= o_gps=
  o_x_cap= o_x_src= o_x_off= o_x_tzd= o_x_camera= o_x_rating= o_x_flag= o_x_tags= o_xmp=
  o_kind= o_note= o_mtime= o_scn= o_unread= o_corrupt=
}

# _opts key=value... : sets o_<key>; unknown keys are a bug in the catalogue.
_opts() {
  local kv k
  for kv in "$@"; do
    case $kv in
      *=*) ;;
      *) fx_die "bad option '$kv' (expected key=value)" ;;
    esac
    k=${kv%%=*}
    case $k in
      fmt | size | seed | noise | tag | opt | \
        dt | cd | sub | offset | make | model | rating | pick | kw | hier | gps | \
        x_cap | x_src | x_off | x_tzd | x_camera | x_rating | x_flag | x_tags | xmp | \
        kind | note | mtime | scn | unread | corrupt)
        printf -v "o_$k" '%s' "${kv#*=}"
        ;;
      *) fx_die "unknown option '$k' in: $*" ;;
    esac
  done
}

# ---------------------------------------------------------------- expectations

# Expectation of a media file that carries no usable metadata: the indexer falls back to the file times.
_exp_fs() {
  e_cap=- e_src=fs e_off=- e_tzd=0 e_camera=- e_rating=0 e_flag=none e_tags=- e_loc=- e_w=- e_h=- e_dur=-
}

# What the indexer should read back from the tags requested through the tag options.
_derive_exp() {
  _exp_fs
  if [ -n "$o_dt" ]; then
    e_cap=$o_dt
    e_src=exif
    if [ -n "$o_offset" ]; then
      fx_offmin "$o_offset"
      e_off=$FX_R
    fi
  elif [ -n "$o_cd" ]; then
    e_cap=$o_cd
    e_src=exif
  fi
  if [ -n "$o_make$o_model" ]; then
    fx_camera "$o_make" "$o_model"
    e_camera=$FX_R
  fi
  if [ -n "$o_rating" ]; then
    if [ "$o_rating" -lt 0 ]; then
      e_rating=0
      e_flag=rejected
    else
      e_rating=$o_rating
    fi
  fi
  case $o_pick in
    1) e_flag=selected ;;
    -1) e_flag=rejected ;;
  esac
  fx_tags "$o_kw" "$o_hier"
  if [ -n "$FX_R" ]; then e_tags=$FX_R; fi
  if [ -n "$o_gps" ]; then e_loc=$o_gps; fi
}

# Sidecar values win field by field over embedded ones (plan A17); an embedded date wins over a sidecar date.
_apply_overrides() {
  if [ "$o_xmp" = 1 ]; then
    if [ "$X_HAS_RATING" = 1 ]; then e_rating=$X_RATING; fi
    if [ "$X_HAS_FLAG" = 1 ]; then e_flag=$X_FLAG; fi
    if [ "$X_HAS_TAGS" = 1 ]; then e_tags=$X_TAGS; fi
    if [ "$X_HAS_CAMERA" = 1 ]; then e_camera=$X_CAMERA; fi
    if [ "$X_HAS_DATE" = 1 ] && [ "$e_cap" = - ]; then
      e_cap=$X_DATE
      e_src=xmp
    fi
  fi
  if [ -n "$o_x_cap" ]; then e_cap=$o_x_cap; fi
  if [ -n "$o_x_src" ]; then e_src=$o_x_src; fi
  if [ -n "$o_x_off" ]; then e_off=$o_x_off; fi
  if [ -n "$o_x_tzd" ]; then e_tzd=$o_x_tzd; fi
  if [ -n "$o_x_camera" ]; then e_camera=$o_x_camera; fi
  if [ -n "$o_x_rating" ]; then e_rating=$o_x_rating; fi
  if [ -n "$o_x_flag" ]; then e_flag=$o_x_flag; fi
  if [ -n "$o_x_tags" ]; then e_tags=$o_x_tags; fi
  case $o_corrupt in
    header | trunc | zero)
      e_cap=- e_src=fs e_off=- e_camera=- e_loc=- e_rating=0 e_flag=none e_tags=-
      ;;
  esac
  # A truncated file ends before the pixel data: no decoder can read its size.
  if [ "$o_corrupt" = trunc ]; then e_w=- e_h=-; fi
}

_exp_store() {
  printf '%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\n' \
    "$e_cap" "$e_src" "$e_off" "$e_tzd" "$e_camera" "$e_rating" "$e_flag" "$e_tags" "$e_loc" "$e_w" "$e_h" "$e_dur" >"$1"
}

# ---------------------------------------------------------------- registry

scn() {
  SCN_KEY=$1
  printf '%s\t%s\t%s\n' "$1" "$2" "$3" >>"$WORK/scenarios.tsv"
}

# skip_scn KEY REASON [VOLUME PATH] : the scenario stays declared but has no file. With a path, a placeholder registry
# row (type "skipped") lets the manifest list the file that would have been created.
skip_scn() {
  local kind dbk
  fx_warn "scenario $1 skipped: $2"
  printf '%s\t%s\n' "$1" "$2" >>"$WORK/skipped.tsv"
  if [ -n "${3:-}" ]; then
    fx_kind_of "${4##*/}"
    kind=$FX_KIND
    dbk=$FX_DBK
    printf '%s\t%s\tskipped\t%s\t%s\t%s\t-\t-\t-\t0\t-\t-\t-\t-\t0\t-\t-\t-\t-\t-\t-\t-\t%s\n' \
      "$3" "$4" "$kind" "$dbk" "$1" "$2" >>"$WORK/reg.tsv"
  fi
}

dir_scn() {
  printf '%s\t%s\t%s\n' "$1" "$2" "${3:-$SCN_KEY}" >>"$WORK/dirscn.tsv"
}

# sc_link VOLUME OWNER SIDECAR exact|stem : a sidecar link the indexer must create (cross-checked by manifest.awk).
sc_link() {
  printf '%s\t%s\t%s\t%s\n' "$1" "$2" "$3" "$4" >>"$WORK/links.tsv"
}

dupgroup() {
  printf '%s\t%s\t%s\t%s\n' "$1" "$2" "$3" "${4:--}" >>"$WORK/dupgroups.tsv"
}

dupsrc() {
  printf '%s\t%s\t%s\t%s\t%s\t%s\n' "$1" "$2" "$3" source - 0 >>"$WORK/dups.tsv"
}

# dupmem GROUP VOLUME PATH MODES [plain] : MODES is the comma list of default-rule modes that must match the source,
# "-" for a negative control; "plain" marks a name that only matches under strict_prefix.
dupmem() {
  printf '%s\t%s\t%s\t%s\t%s\t%s\n' "$1" "$2" "$3" member "${4:--}" "$([ "${5:-}" = plain ] && echo 1 || echo 0)" >>"$WORK/dups.tsv"
}

_mkparent() {
  local d=${1%/*}
  if [ ! -d "$d" ]; then mkdir -p -- "$d"; fi
}

# _register VOLUME PATH file|symlink [LINK_TARGET] : appends the registry row and queues the mtime.
_register() {
  local vol=$1 rel=$2 type=$3 link=${4:--} kind dbk scn idx=1 skip=- unread=- mt
  fx_kind_of "${rel##*/}"
  kind=$FX_KIND
  dbk=$FX_DBK
  if [ "$o_kind" = junk ]; then
    kind=junk
    dbk=-
  fi
  scn=${o_scn:-$SCN_KEY}
  if [ -z "$scn" ]; then fx_die "no scenario declared before $vol/$rel"; fi
  if [ "$kind" = junk ]; then
    idx=0
    skip=junk
  fi
  if [ "$type" = symlink ]; then
    kind=other
    dbk=-
    idx=0
    skip=symlink
  fi
  case $o_unread in
    file)
      unread=file
      printf '%s\n' "volumes/$vol/$rel" >>"$WORK/unreadable.list"
      ;;
    dir)
      unread=dir
      idx=0
      skip=unreadable_dir
      printf '%s\n' "volumes/$vol/${rel%/*}" >>"$WORK/unreadable.list"
      ;;
  esac
  printf '%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\n' \
    "$vol" "$rel" "$type" "$kind" "$dbk" "$scn" \
    "${e_cap:--}" "${e_src:--}" "${e_off:--}" "${e_tzd:-0}" "${e_camera:--}" \
    "${e_rating:--}" "${e_flag:--}" "${e_tags:--}" "$idx" "${o_note:--}" "$unread" \
    "$link" "${e_w:--}" "${e_h:--}" "${e_dur:--}" "${e_loc:--}" "$skip" >>"$WORK/reg.tsv"
  if [ "$type" = file ]; then
    mt=$o_mtime
    if [ -z "$mt" ]; then
      if [ "${e_cap:--}" != - ]; then mt=$e_cap; else mt=$DEFAULT_MTIME; fi
    fi
    fx_stamp "$mt"
    printf '%s\t%s\n' "$FX_R" "$OUT/volumes/$vol/$rel" >>"$WORK/touch.tsv"
  fi
}

# can_name NAME : succeeds when the filesystem under OUT stores this exact file name.
can_name() {
  local probe="$WORK/probe/$1" ok=1
  if ! { : >"$probe"; } 2>/dev/null; then ok=0; fi
  if [ "$ok" = 1 ] && ! ls -1 "$WORK/probe" | grep -qxF -- "$1"; then ok=0; fi
  rm -f -- "$probe" 2>/dev/null || true
  [ "$ok" = 1 ]
}

# ---------------------------------------------------------------- image synthesis

# _jpeg SEED WxH OUT NOISE : a deterministic JPEG. Patterns are sine plaids chosen from SEED; NOISE=1 gives ~1.5 MB of noise at 1920x1280.
_jpeg() {
  local s=$1 size=$2 out=$3 noise=$4 a b c d filt q
  if [ "$noise" = 1 ]; then
    filt="nullsrc=s=${size},format=rgb24,geq=r='random(1)*255':g='random(2)*255':b='random(3)*255'"
    q=3
  else
    a=$((2 + (s * 7) % 9))
    b=$((1 + (s * 5) % 7))
    c=$(((s * 37) % 628))
    d=$((3 + (s * 11) % 8))
    filt="nullsrc=s=${size},format=gbrp,geq=r='128+120*sin(X*$a/W*3.1416+Y*$b/H*3.1416+$c/100)':g='128+120*sin(X*$b/W*3.1416-Y*$d/H*3.1416+$c/50)':b='128+120*sin((X+Y)*$d/W*3.1416+$c/30)'"
    q=4
  fi
  ffmpeg -nostdin -hide_banner -loglevel error -y -fflags +bitexact -flags:v +bitexact \
    -f lavfi -i "$filt" -frames:v 1 -update 1 -q:v "$q" "$out" >/dev/null 2>&1
}

_ffmpeg_still() {
  ffmpeg -nostdin -hide_banner -loglevel error -y -i "$1" -frames:v 1 -update 1 "$2" >/dev/null 2>&1
}

# _make_image SEED SIZE FMT OUT NOISE : jpg|png|gif|webp|tif|heic. Call it from an `if` (it returns the converter's status).
_make_image() {
  local seed=$1 size=$2 fmt=$3 out=$4 noise=$5 base rc=0
  if [ "$fmt" = jpg ]; then
    _jpeg "$seed" "$size" "$out" "$noise"
    return
  fi
  base="$out.src.jpg"
  _jpeg "$seed" "$size" "$base" "$noise" || return 1
  case $fmt in
    heic)
      if command -v heif-enc >/dev/null 2>&1; then
        heif-enc -q 50 -o "$out" "$base" >/dev/null 2>&1 || rc=1
      else
        rc=1
      fi
      ;;
    webp)
      vips copy "$base" "$out" >/dev/null 2>&1 || rc=1
      ;;
    png | tif | gif)
      vips copy "$base" "$out" >/dev/null 2>&1 || _ffmpeg_still "$base" "$out" || rc=1
      ;;
    *)
      rc=1
      ;;
  esac
  rm -f -- "$base"
  return $rc
}

# _fmt_for_ext EXT : staging format for a destination extension (RAW stand-ins reuse JPEG or TIFF content).
_fmt_for_ext() {
  local nc=0
  if shopt -q nocasematch; then nc=1; fi
  shopt -s nocasematch
  case $1 in
    jpg | jpeg | cr2 | cr3 | crw) FX_R=jpg ;;
    png) FX_R=png ;;
    gif) FX_R=gif ;;
    webp) FX_R=webp ;;
    tif | tiff | nef | nrw | arw | dng | raf | orf | rw2 | pef) FX_R=tif ;;
    heic | heif) FX_R=heic ;;
    *) FX_R=jpg ;;
  esac
  if [ "$nc" = 0 ]; then shopt -u nocasematch; fi
}

# _exif_block FILE : appends one exiftool argfile block writing the tag options onto FILE.
_exif_block() {
  local file=$1 item lat lon latref lonref IFS_SAVE=$IFS
  if [ -z "$o_dt$o_cd$o_sub$o_offset$o_make$o_model$o_rating$o_pick$o_kw$o_hier$o_gps" ]; then return 0; fi
  {
    printf '%s\n' -q -q -m -overwrite_original
    if [ -n "$o_dt" ]; then
      fx_exif_dt "$o_dt"
      printf '%s\n' "-DateTimeOriginal=$FX_R"
    fi
    if [ -n "$o_cd" ]; then
      fx_exif_dt "$o_cd"
      printf '%s\n' "-CreateDate=$FX_R"
    fi
    if [ -n "$o_sub" ]; then printf '%s\n' "-SubSecTimeOriginal=$o_sub"; fi
    if [ -n "$o_offset" ]; then printf '%s\n' "-OffsetTimeOriginal=$o_offset"; fi
    if [ -n "$o_make" ]; then printf '%s\n' "-Make=$o_make"; fi
    if [ -n "$o_model" ]; then printf '%s\n' "-Model=$o_model"; fi
    if [ -n "$o_rating" ]; then printf '%s\n' "-EXIF:Rating=$o_rating" "-XMP:Rating=$o_rating"; fi
    if [ -n "$o_pick" ]; then printf '%s\n' "-XMP-xmpDM:Pick=$o_pick"; fi
    if [ -n "$o_kw" ]; then
      IFS=,
      for item in $o_kw; do
        printf '%s\n' "-XMP-dc:Subject+=$item"
      done
      IFS=$IFS_SAVE
    fi
    if [ -n "$o_hier" ]; then printf '%s\n' "-XMP-lr:HierarchicalSubject+=$o_hier"; fi
    if [ -n "$o_gps" ]; then
      lat=${o_gps%,*}
      lon=${o_gps#*,}
      latref=N
      lonref=E
      case $lat in -*)
        latref=S
        lat=${lat#-}
        ;;
      esac
      case $lon in -*)
        lonref=W
        lon=${lon#-}
        ;;
      esac
      printf '%s\n' "-GPSLatitude=$lat" "-GPSLatitudeRef=$latref" "-GPSLongitude=$lon" "-GPSLongitudeRef=$lonref"
    fi
    printf '%s\n' "$file" -execute
  } >>"$WORK/exiftool.args"
}

# _stage_image REL : renders one image into the stage directory (tags queued for the exiftool batch).
# Sets STAGE_OK (0 when an optional format could not be produced) and FX_KEY (the stage file).
_stage_image() {
  local rel=$1 ext fmt size seed n file w h
  STAGE_OK=1
  ext=${rel##*.}
  if [ -n "$o_fmt" ]; then
    fmt=$o_fmt
  else
    _fmt_for_ext "$ext"
    fmt=$FX_R
  fi
  size=${o_size:-160x120}
  if [ -n "$o_seed" ]; then
    seed=$o_seed
  else
    SEED=$((SEED + 1))
    seed=$SEED
  fi
  STAGE_N=$((STAGE_N + 1))
  printf -v n '%05d' "$STAGE_N"
  file="$STAGE/$n.$fmt"
  if ! _make_image "$seed" "$size" "$fmt" "$file" "$o_noise"; then
    rm -f -- "$file"
    if [ "$o_opt" = 1 ]; then
      STAGE_OK=0
      STAGE_REASON="cannot produce $fmt images here (encoder missing or failed)"
      return 0
    fi
    fx_die "cannot create a $fmt image for $rel"
  fi
  w=${size%x*}
  h=${size#*x}
  if [ "$o_tag" = 0 ] || [ "$fmt" = gif ]; then
    _exp_fs
  else
    _derive_exp
    _exif_block "$file"
  fi
  e_w=$w e_h=$h e_dur=-
  _exp_store "$STAGE/$n.exp"
  FX_KEY=$file
}

# _place KEY VOLUME PATH : queues the copy of a staged file to its destination and registers it.
_place() {
  local key=$1 vol=$2 rel=$3 dest
  dest="$OUT/volumes/$vol/$rel"
  IFS=$'\t' read -r e_cap e_src e_off e_tzd e_camera e_rating e_flag e_tags e_loc e_w e_h e_dur <"${key%.*}.exp"
  _apply_overrides
  _mkparent "$dest"
  printf '%s\t%s\t%s\n' "$key" "$dest" "${o_corrupt:--}" >>"$WORK/copies.tsv"
  _register "$vol" "$rel" file
}

# ---------------------------------------------------------------- public file helpers

# img VOLUME PATH [key=value...] : render, tag and place a new image. Sets FX_KEY so put can place byte-identical copies.
img() {
  local vol=$1 rel=$2
  shift 2
  _reset_opts
  _opts "$@"
  _stage_image "$rel"
  if [ "$STAGE_OK" = 1 ]; then
    _place "$FX_KEY" "$vol" "$rel"
  else
    skip_scn "$SCN_KEY" "$STAGE_REASON" "$vol" "$rel"
  fi
}

# put KEY VOLUME PATH [key=value...] : another placement of a staged image (identical bytes).
put() {
  local key=$1 vol=$2 rel=$3
  shift 3
  _reset_opts
  _opts "$@"
  _place "$key" "$vol" "$rel"
}

# _prep VOLUME PATH : sets dest and creates the parent directory.
_prep() {
  dest="$OUT/volumes/$1/$2"
  _mkparent "$dest"
}

put_empty() {
  local vol=$1 rel=$2 dest
  shift 2
  _reset_opts
  _opts "$@"
  _prep "$vol" "$rel"
  : >"$dest"
  _exp_fs
  _apply_overrides
  _register "$vol" "$rel" file
}

put_text() {
  local vol=$1 rel=$2 text=$3 dest
  shift 3
  _reset_opts
  _opts "$@"
  _prep "$vol" "$rel"
  printf '%s\n' "$text" >"$dest"
  _exp_fs
  _register "$vol" "$rel" file
}

# put_bytes VOLUME PATH PRINTF_FORMAT [key=value...] : binary content given as \xHH escapes.
put_bytes() {
  local vol=$1 rel=$2 fmt=$3 dest
  shift 3
  _reset_opts
  _opts "$@"
  _prep "$vol" "$rel"
  # shellcheck disable=SC2059
  printf "$fmt" >"$dest"
  _exp_fs
  _register "$vol" "$rel" file
}

put_junk() {
  local vol=$1 rel=$2 fmt=$3
  put_bytes "$vol" "$rel" "$fmt" kind=junk
}

# put_xmp VOLUME PATH [fx_write_xmp options] : writes an XMP sidecar (see common.sh) in the current scenario.
put_xmp() {
  local vol=$1 rel=$2 dest
  shift 2
  _reset_opts
  _prep "$vol" "$rel"
  fx_write_xmp "$dest" "$@"
  _exp_fs
  _register "$vol" "$rel" file
}

mk_symlink() {
  local vol=$1 rel=$2 target=$3 dest
  shift 3
  _reset_opts
  _opts "$@"
  _prep "$vol" "$rel"
  ln -s -- "$target" "$dest"
  _exp_fs
  _register "$vol" "$rel" symlink "$target"
}

empty_dir() {
  mkdir -p -- "$OUT/volumes/$1/$2"
  dir_scn "$1" "$2" "${3:-$SCN_KEY}"
}

# ---------------------------------------------------------------- videos

_ffmpeg_video() {
  local dest=$1
  shift
  ffmpeg -nostdin -hide_banner -loglevel error -y -f lavfi -i "testsrc=size=96x64:rate=10:duration=1" \
    -c:v libx264 -preset ultrafast -pix_fmt yuv420p -fflags +bitexact -flags:v +bitexact -threads 1 "$@" "$dest" >/dev/null 2>&1
}

# video VOLUME PATH mp4|mov|nometa [key=value...]
video() {
  local vol=$1 rel=$2 vk=$3 dest rc=0
  shift 3
  _reset_opts
  _opts "$@"
  _prep "$vol" "$rel"
  _exp_fs
  e_w=96 e_h=64 e_dur=1000
  case $vk in
    mp4)
      _ffmpeg_video "$dest" -metadata creation_time=2022-08-15T10:20:30Z -movflags +faststart || rc=1
      e_cap=2022-08-15T10:20:30 e_src=video e_tzd=1
      ;;
    mov)
      _ffmpeg_video "$dest" -metadata creation_time=2023-11-20T13:05:09Z \
        -metadata com.apple.quicktime.creationdate=2023-11-20T14:05:09+0100 -movflags use_metadata_tags -f mov || rc=1
      e_cap=2023-11-20T14:05:09 e_off=60 e_src=video
      ;;
    nometa)
      _ffmpeg_video "$dest" -map_metadata -1 -movflags +faststart || rc=1
      ;;
    *) fx_die "video: unknown kind '$vk'" ;;
  esac
  if [ "$rc" != 0 ]; then
    rm -f -- "$dest"
    if [ "$o_opt" = 1 ]; then
      skip_scn "$SCN_KEY" "ffmpeg could not encode the test video (libx264 missing?)" "$vol" "$rel"
      return 0
    fi
    fx_die "ffmpeg failed to create $rel"
  fi
  _apply_overrides
  _register "$vol" "$rel" file
}

# ---------------------------------------------------------------- dated JPEG templates (grid and large scale)

# tpl_init : builds 4 cameras x 8 pixel variants of a 16x12 JPEG whose DateTimeOriginal is a sentinel, then derives a printf
# format prefix/suffix per template. A file is written by printing prefix, the date and suffix: no process per file.
tpl_init() {
  local v k make model idx f hex sent_hex pre suf
  for v in 0 1 2 3 4 5 6 7; do
    _jpeg "$((v + 101))" 16x12 "$STAGE/tiny$v.jpg" 0 || fx_die "ffmpeg failed to render the tiny JPEG variants"
  done
  : >"$WORK/tpl.args"
  for k in 0 1 2 3; do
    make=${TPL_CAMS[$k]%%|*}
    model=${TPL_CAMS[$k]#*|}
    for v in 0 1 2 3 4 5 6 7; do
      f="$STAGE/tpl_${k}_$v.jpg"
      cp "$STAGE/tiny$v.jpg" "$f"
      printf '%s\n' -q -q -m -overwrite_original "-DateTimeOriginal=$TPL_SENTINEL" "-Make=$make" "-Model=$model" "$f" -execute >>"$WORK/tpl.args"
    done
  done
  if ! exiftool -@ "$WORK/tpl.args" >"$WORK/tpl.out" 2>&1 || [ -s "$WORK/tpl.out" ]; then
    fx_die "exiftool failed while building the JPEG templates: $(head -n 5 "$WORK/tpl.out")"
  fi
  sent_hex=$(printf '%s' "$TPL_SENTINEL" | od -An -v -tx1 | tr -d ' \n')
  for k in 0 1 2 3; do
    for v in 0 1 2 3 4 5 6 7; do
      f="$STAGE/tpl_${k}_$v.jpg"
      idx=$((k * 8 + v))
      hex=$(od -An -v -tx1 "$f" | tr -d ' \n')
      pre=${hex%%"$sent_hex"*}
      if [ "$pre" = "$hex" ] || [ $((${#pre} % 2)) != 0 ]; then
        fx_die "template $f does not contain the date sentinel at a byte boundary"
      fi
      suf=${hex#*"$sent_hex"}
      fx_hex_fmt "$pre"
      TPL_PRE[$idx]=$FX_R
      fx_hex_fmt "$suf"
      TPL_SUF[$idx]=$FX_R
    done
  done
  # shellcheck disable=SC2059
  printf "${TPL_PRE[0]}%s${TPL_SUF[0]}" "$TPL_SENTINEL" >"$STAGE/tpl_check.jpg"
  cmp -s "$STAGE/tpl_check.jpg" "$STAGE/tpl_0_0.jpg" || fx_die "JPEG template round trip failed"
}

# put_dated VOLUME PATH CAMERA_INDEX VARIANT ISO_DATE [key=value...] : writes a 16x12 JPEG with the given
# DateTimeOriginal and camera straight to its destination (template based, see tpl_init).
put_dated() {
  local vol=$1 rel=$2 k=$3 v=$4 iso=$5 idx dest dt make model
  shift 5
  _reset_opts
  _opts "$@"
  _prep "$vol" "$rel"
  idx=$((k * 8 + v))
  fx_exif_dt "$iso"
  dt=$FX_R
  # shellcheck disable=SC2059
  printf "${TPL_PRE[$idx]}%s${TPL_SUF[$idx]}" "$dt" >"$dest"
  make=${TPL_CAMS[$k]%%|*}
  model=${TPL_CAMS[$k]#*|}
  fx_camera "$make" "$model"
  _exp_fs
  e_cap=$iso e_src=exif e_camera=$FX_R e_w=16 e_h=12
  _apply_overrides
  _register "$vol" "$rel" file
}

# ---------------------------------------------------------------- batch steps

media_run_exiftool() {
  local args=$WORK/exiftool.args out=$WORK/exiftool.out
  if [ ! -s "$args" ]; then return 0; fi
  if ! exiftool -@ "$args" >"$out" 2>&1 || [ -s "$out" ]; then
    fx_die "exiftool reported problems while tagging the staged files:
$(head -n 20 "$out")"
  fi
}

# exif_marker FILE : sets FX_R to the byte offset of the "Exif\0\0" marker of the APP1 segment (found by scanning the
# first bytes, so the recipes below survive a different JPEG layout from another ffmpeg or exiftool version).
exif_marker() {
  local hex pre
  hex=$(od -An -v -tx1 -N 256 -- "$1" | tr -d ' \n')
  case $hex in
    *45786966*) ;;
    *) fx_die "no EXIF segment found at the start of $1" ;;
  esac
  pre=${hex%%45786966*}
  if [ $((${#pre} % 2)) != 0 ]; then fx_die "unexpected byte layout at the start of $1"; fi
  FX_R=$((${#pre} / 2))
}

media_run_copies() {
  local src dst act
  while IFS=$'\t' read -r src dst act; do
    case $act in
      header)
        # The IFD0 offset of the TIFF header (marker + 6 + 4) becomes 0xFFFFFFF0: exiftool warns "Bad IFD0 directory"
        # and reads no tag at all.
        exif_marker "$src"
        cp -- "$src" "$dst"
        printf '\377\377\377\360' | dd of="$dst" bs=1 seek=$((FX_R + 10)) conv=notrunc 2>/dev/null
        ;;
      trunc)
        # Cut 36 bytes after the marker, inside the first IFD entries: exiftool warns "JPEG format error".
        exif_marker "$src"
        head -c $((FX_R + 36)) "$src" >"$dst"
        ;;
      *)
        cp -- "$src" "$dst"
        ;;
    esac
  done <"$WORK/copies.tsv"
}

# media_apply_touch LIST : LIST rows are "STAMP<TAB>PATH"; one touch process per distinct stamp.
media_apply_touch() {
  local list=$1 dir="$WORK/touch.d" f stamp
  rm -rf "$dir"
  mkdir -p "$dir"
  sort -t $'\t' -k1,1 -s "$list" | awk -F'\t' -v d="$dir" '
    $1 != prev { if (prev != "") close(f); prev = $1; f = d "/" $1 }
    { print $2 > f }'
  for f in "$dir"/*; do
    if [ ! -f "$f" ]; then continue; fi
    stamp=${f##*/}
    tr '\n' '\0' <"$f" | TZ=UTC xargs -0 -r touch -t "$stamp" --
  done
}
