# shellcheck shell=bash
# Large-scale catalogue: FILES tiny files over about FILES/150 folders (see bulk.awk for the plan).
# Sourced by generate.sh after media.sh. Expects OUT, WORK, FILES, JOBS and an initialised template set (tpl_init).
#
# Files are written with the printf builtin from pre-tagged JPEG templates, so no process is spawned per file
# and exiftool only ever touches the 32 templates. The work is split over JOBS background writers.

BULK_XMP=()

# bulk_write_shard LIST : LIST rows are "template<TAB>EXIF date<TAB>absolute path".
bulk_write_shard() {
  local list=$1 idx dt path
  while IFS=$'\t' read -r idx dt path; do
    # shellcheck disable=SC2059
    printf "${TPL_PRE[$idx]}%s${TPL_SUF[$idx]}" "$dt" >"$path"
  done <"$list"
}

# bulk_write_xmps LIST : LIST rows are "rating<TAB>absolute path".
bulk_write_xmps() {
  local list=$1 rating path
  while IFS=$'\t' read -r rating path; do
    # shellcheck disable=SC2059
    printf "${BULK_XMP[$rating]}" >"$path"
  done <"$list"
}

bulk_plan() {
  local d="$WORK/bulk" s r pid t0 fail=0 summary c0 c1 c2 c3
  local pids=()
  mkdir -p "$d"
  scn bulk.unique bulk "Unique 16x12 JPEG with a deterministic capture date (2015-2024) and one of four cameras"
  scn bulk.dup_hash bulk "Mode 1 duplicate of a bulk.unique photo: identical bytes, other folder and name (5% of the unique photos)"
  scn bulk.dup_name_time bulk "Mode 2 duplicate of a bulk.unique photo: same name and capture second, other pixels (5% of the unique photos)"
  scn bulk.sidecar bulk "XMP sidecar with a rating, linked alternately by stem and by exact name (2% of the unique photos)"
  dir_scn photos bulk bulk.unique
  dir_scn archive bulk1 bulk.dup_hash
  dir_scn archive bulk2 bulk.dup_name_time

  fx_camera "${TPL_CAMS[0]%%|*}" "${TPL_CAMS[0]#*|}"
  c0=$FX_R
  fx_camera "${TPL_CAMS[1]%%|*}" "${TPL_CAMS[1]#*|}"
  c1=$FX_R
  fx_camera "${TPL_CAMS[2]%%|*}" "${TPL_CAMS[2]#*|}"
  c2=$FX_R
  fx_camera "${TPL_CAMS[3]%%|*}" "${TPL_CAMS[3]#*|}"
  c3=$FX_R

  t0=$SECONDS
  summary=$(awk -v N="$FILES" -v OUTV="$OUT/volumes" -v P="$JOBS" -v C0="$c0" -v C1="$c1" -v C2="$c2" -v C3="$c3" \
    -v REG="$WORK/reg.tsv" -v LINKS="$WORK/links.tsv" -v GRP="$WORK/dupgroups.tsv" -v DUPS="$WORK/dups.tsv" \
    -v DIRSOUT="$d/dirs.list" -v SHARD="$d/shard" -v XMPS="$d/xmp" -f "$FX_LIB_DIR/bulk.awk")
  fx_log "bulk plan: $summary"
  (cd "$OUT/volumes" && tr '\n' '\0' <"$d/dirs.list" | xargs -0 -r mkdir -p --)
  fx_log "bulk plan and folders: $((SECONDS - t0))s"

  t0=$SECONDS
  for ((s = 0; s < JOBS; s++)); do
    if [ -f "$d/shard.$s" ]; then
      bulk_write_shard "$d/shard.$s" &
      pids[${#pids[@]}]=$!
    fi
  done
  for r in 1 2 3 4 5; do
    fx_write_xmp "$d/rating$r.xmp" rating=$r
    BULK_XMP[$r]=$(fx_printf_fmt "$d/rating$r.xmp")
  done
  for ((s = 0; s < JOBS; s++)); do
    if [ -f "$d/xmp.$s" ]; then bulk_write_xmps "$d/xmp.$s"; fi
  done
  for pid in ${pids[@]+"${pids[@]}"}; do
    wait "$pid" || fail=1
  done
  if [ "$fail" != 0 ]; then fx_die "a bulk writer failed"; fi
  fx_log "bulk files written: $((SECONDS - t0))s with $JOBS writers"

  t0=$SECONDS
  find "$OUT/volumes" -type f -exec env TZ=UTC touch -t 202001011200.00 {} +
  fx_log "bulk mtimes set: $((SECONDS - t0))s"
  rm -rf "$d"
}
