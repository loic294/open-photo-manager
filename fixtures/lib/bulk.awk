# bulk.awk: expands the large-scale plan into file lists and registry rows (POSIX awk, no gawk extensions).
#
# Plan, for u = 0 .. n-1 unique photos (n is the largest value whose total file count stays within N):
#   photos/bulk/YYYY/YYYY-MM-DD_gNNNNN/IMG_<u+1>.jpg   150 photos per folder, capture dates spread evenly over ten years
#   u%20==0   mode 1 duplicate  archive/bulk1/aNNNN/COPY_<u+1>.jpg   identical bytes
#   u%20==7   mode 2 duplicate  archive/bulk2/aNNNN/IMG_<u+1>.jpg    same name and capture second, other pixels
#   u%50==11  XMP sidecar next to the photo (alternating stem and exact link) carrying rating 1..5
# One archive folder serves eight photo folders, so the whole tree has about N/130 folders.
# Camera = u%4 and pixel variant = u%8 select one of the 32 JPEG templates (see tpl_init in media.sh).
#
# -v inputs: N OUTV P C0 C1 C2 C3 REG LINKS GRP DUPS DIRSOUT SHARD XMPS
# Output: registry/link/group rows appended to REG, LINKS, GRP, DUPS; DIRSOUT lists directories to create;
# SHARD.<s> lines "template<TAB>exif date<TAB>path" and XMPS.<s> lines "rating<TAB>path" (s = folder number % P).

function ceil_div(a, b) { return a <= 0 ? 0 : int((a + b - 1) / b) }
function total(n) { return n + ceil_div(n, 20) + ceil_div(n - 7, 20) + ceil_div(n - 11, 50) }

# Epoch seconds to calendar fields C_Y C_M C_D C_h C_m C_s (proleptic Gregorian, UTC).
function civil(e,   days, sec, z, era, doe, yoe, doy, mp) {
  days = int(e / 86400)
  sec = e - days * 86400
  z = days + 719468
  era = int(z / 146097)
  doe = z - era * 146097
  yoe = int((doe - int(doe / 1460) + int(doe / 36524) - int(doe / 146096)) / 365)
  C_Y = yoe + era * 400
  doy = doe - (365 * yoe + int(yoe / 4) - int(yoe / 100))
  mp = int((5 * doy + 2) / 153)
  C_D = doy - int((153 * mp + 2) / 5) + 1
  C_M = mp < 10 ? mp + 3 : mp - 9
  if (C_M <= 2) C_Y++
  C_h = int(sec / 3600)
  C_m = int((sec - C_h * 3600) / 60)
  C_s = sec - C_h * 3600 - C_m * 60
}

function row(vol, rel, kind, dbk, scn, cap, cam, rating, w, h) {
  printf "%s\t%s\tfile\t%s\t%s\t%s\t%s\t%s\t-\t0\t%s\t%s\tnone\t-\t1\t-\t-\t-\t%s\t%s\t-\t-\t-\n",
    vol, rel, kind, dbk, scn, cap, (kind == "image" ? "exif" : "-"), cam, rating, w, h >> REG
}

BEGIN {
  BASE = 1420070400
  SPAN = 315360000
  N += 0
  P += 0
  CAM[0] = C0; CAM[1] = C1; CAM[2] = C2; CAM[3] = C3
  n = int(N / 1.12)
  while (total(n + 1) <= N) n++
  while (n > 1 && total(n) > N) n--
  n1 = n2 = nx = 0
  for (u = 0; u < n; u++) {
    epoch = BASE + int(u * SPAN / n)
    civil(epoch)
    iso = sprintf("%04d-%02d-%02dT%02d:%02d:%02d", C_Y, C_M, C_D, C_h, C_m, C_s)
    exd = sprintf("%04d:%02d:%02d %02d:%02d:%02d", C_Y, C_M, C_D, C_h, C_m, C_s)
    if (u % 150 == 0) {
      g = u / 150
      ag = int(g / 8)
      aname = sprintf("a%04d", ag)
      gname = sprintf("g%05d", g)
      pdir = sprintf("bulk/%04d/%04d-%02d-%02d_%s", C_Y, C_Y, C_M, C_D, gname)
      print "photos/" pdir > DIRSOUT
      sh = g % P
    }
    k = u % 4
    v = u % 8
    tpl = k * 8 + v
    num = sprintf("%06d", u + 1)
    rel = pdir "/IMG_" num ".jpg"
    rating = 0
    if (u % 50 == 11) {
      s = (u - 11) / 50
      rating = 1 + s % 5
      if (s % 2 == 0) { xrel = pdir "/IMG_" num ".xmp"; lk = "stem" } else { xrel = pdir "/IMG_" num ".jpg.xmp"; lk = "exact" }
      printf "%s\t%s\n", rating, OUTV "/photos/" xrel > (XMPS "." sh)
      row("photos", xrel, "sidecar", "Sidecar", "bulk.sidecar", "-", "-", 0, "-", "-")
      printf "photos\t%s\t%s\t%s\n", rel, xrel, lk >> LINKS
      nx++
    }
    printf "%s\t%s\t%s\n", tpl, exd, OUTV "/photos/" rel > (SHARD "." sh)
    row("photos", rel, "image", "Image", "bulk.unique", iso, CAM[k], rating, 16, 12)
    if (u % 20 == 0) {
      if (!(ag in SEEN1)) { SEEN1[ag] = 1; nf1++; print "archive/bulk1/" aname > DIRSOUT }
      crel = "bulk1/" aname "/COPY_" num ".jpg"
      printf "%s\t%s\t%s\n", tpl, exd, OUTV "/archive/" crel > (SHARD "." sh)
      row("archive", crel, "image", "Image", "bulk.dup_hash", iso, CAM[k], 0, 16, 12)
      printf "b1_%s\t1\tmode 1 copy of IMG_%s.jpg\t-\n", num, num >> GRP
      printf "b1_%s\tphotos\t%s\tsource\t-\t0\n", num, rel >> DUPS
      printf "b1_%s\tarchive\t%s\tmember\t1\t0\n", num, crel >> DUPS
      n1++
    }
    if (u % 20 == 7) {
      if (!(ag in SEEN2)) { SEEN2[ag] = 1; nf2++; print "archive/bulk2/" aname > DIRSOUT }
      crel = "bulk2/" aname "/IMG_" num ".jpg"
      printf "%s\t%s\t%s\n", k * 8 + (v + 1) % 8, exd, OUTV "/archive/" crel > (SHARD "." sh)
      row("archive", crel, "image", "Image", "bulk.dup_name_time", iso, CAM[k], 0, 16, 12)
      printf "b2_%s\t2\tmode 2 copy of IMG_%s.jpg (same name and capture second, other pixels)\t-\n", num, num >> GRP
      printf "b2_%s\tphotos\t%s\tsource\t-\t0\n", num, rel >> DUPS
      printf "b2_%s\tarchive\t%s\tmember\t2,4\t0\n", num, crel >> DUPS
      n2++
    }
  }
  printf "unique=%d mode1=%d mode2=%d sidecars=%d total=%d folders=%d\n", n, n1, n2, nx, n + n1 + n2 + nx, int((n + 149) / 150) + nf1 + nf2
}
