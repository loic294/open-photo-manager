# shellcheck shell=bash
# The small-scale catalogue: one block per scenario group of the brief. Every scenario has a stable key
# (group.name) that tests select on through manifest.json. Sourced by generate.sh after media.sh.
#
# Conventions: unique file stems everywhere except where a duplicate group is declared on purpose (the manifest
# builder cross-checks every pair of media files against the declared groups); one embedded date OR one sidecar date
# per file, never two that differ; photos/ and archive/ are the only volumes.

GRID_FILES=500

small_catalogue() {
  _small_exif
  _small_media
  _small_sidecars
  _small_dups
  _small_names
  _small_junk
  _small_empty
  _small_symlinks
  _small_deep
  _small_stale
  _small_perms
}

_small_exif() {
  scn exif.full_nikon exif "Complete EXIF: Nikon D850 with DateTimeOriginal and SubSecTimeOriginal (the fraction is truncated)"
  img photos 2021/03/DSC_0001.jpg dt=2021-03-14T09:26:53 sub=250 make=NIKON model=D850
  scn exif.dec31_offset exif "Year boundary 23:59:59 with an explicit -05:00 OffsetTimeOriginal"
  img photos 2022/12/IMG_1101.jpg dt=2022-12-31T23:59:59 offset=-05:00 make=Canon model="Canon EOS R5"
  scn exif.jan1_offset exif "Midnight on January 1st with +02:00 OffsetTimeOriginal; model without the make prefix"
  img photos 2023/01/IMG_1102.jpg dt=2023-01-01T00:00:00 offset=+02:00 make=Canon model="EOS R5"
  scn exif.iphone_subsec exif "iPhone 15 Pro with SubSecTimeOriginal 987 (must not round up to the next second) and +02:00 offset"
  img photos 2023/06/IMG_1103.jpg dt=2023-06-15T18:42:07 sub=987 offset=+02:00 make=Apple model="iPhone 15 Pro"
  scn exif.leap_day_sony exif "Leap day capture from a Sony body"
  img photos 2024/02/IMG_1104.jpg dt=2024-02-29T12:00:00 make=Sony model=ILCE-7M4
  scn exif.make_prefix_nikon exif "Model that repeats the make (NIKON Z 7) is not prefixed twice"
  img photos 2025/08/DSC_0002.jpg dt=2025-08-09T07:15:30 make=NIKON model="NIKON Z 7"
  scn exif.make_case_canon exif "Upper-case make with a mixed-case make prefix in the model (CANON / Canon EOS 5D Mark IV)"
  img photos 2026/01/IMG_1105.jpg dt=2026-01-02T20:00:00 make=CANON model="Canon EOS 5D Mark IV"
  scn exif.no_camera exif "Capture date but no camera make or model"
  img photos no_camera/IMG_1106.jpg dt=2020-05-05T05:05:05
  scn exif.no_date exif "Camera but no capture date: the capture time falls back to the file time (birth time, not asserted)"
  img photos no_date/IMG_1107.jpg make=Canon model="Canon EOS R5" mtime=2019-07-04T10:00:00
  scn exif.create_date_only exif "Only CreateDate is present (no DateTimeOriginal); assumed to still count as EXIF"
  img photos 2023/07/IMG_1108.jpg cd=2023-07-07T07:07:07
  scn exif.rating_pick_keywords exif "Embedded XMP: rating 3, pick (selected), keywords Family/Holiday and hierarchical subject Travel|Spain|Madrid"
  img photos 2022/08/IMG_1109.jpg dt=2022-08-20T11:11:11 rating=3 pick=1 kw=Family,Holiday hier='Travel|Spain|Madrid'
  scn exif.rating_rejected exif "Embedded rating -1: stored as rating 0 with the rejected flag (assumption, see README)"
  img photos 2022/08/IMG_1110.jpg dt=2022-08-21T11:11:11 rating=-1
  scn exif.gps_paris exif "GPS in the northern and eastern hemispheres (48.8584, 2.2945)"
  img photos places/IMG_1111.jpg dt=2023-09-09T12:00:00 gps=48.8584,2.2945
  scn exif.gps_sydney exif "GPS in the southern and eastern hemispheres (-33.8688, 151.2093)"
  img photos places/IMG_1112.jpg dt=2023-10-10T12:00:00 gps=-33.8688,151.2093
  scn exif.corrupt_header exif "IFD0 offset of the EXIF header overwritten (exiftool: Bad IFD0 directory): no metadata is readable, capture falls back to the file time"
  img photos broken/exif_bad_header.jpg dt=2021-01-01T10:00:00 make=Canon model="Canon EOS R5" corrupt=header mtime=2018-08-08T08:08:08
  scn exif.truncated exif "JPEG cut off inside the EXIF segment (exiftool: JPEG format error): no metadata is readable"
  img photos broken/exif_truncated.jpg dt=2021-01-02T10:00:00 make=Canon model="Canon EOS R5" corrupt=trunc mtime=2018-08-09T08:08:08
  scn exif.zero_byte exif "Zero-byte file with a .jpg name"
  put_empty photos broken/zero_byte.jpg mtime=2018-08-10T08:08:08
  scn exif.large_file exif "More than 1 MB of noise at 1920x1280 (hashing and thumbnail stress)"
  img photos big/large_noise.jpg size=1920x1280 noise=1 dt=2022-02-02T14:00:00 make=Fujifilm model=X-T5
}

_small_media() {
  local m=media
  scn media.png media "PNG with an eXIf chunk"
  img photos $m/media_png.png dt=2021-07-01T10:00:00 make=Canon model="Canon EOS R5"
  scn media.gif media "GIF cannot carry EXIF: capture falls back to the file time"
  img photos $m/media_gif.gif mtime=2021-07-02T10:00:00 opt=1
  scn media.webp media "WebP with EXIF"
  img photos $m/media_webp.webp dt=2021-07-03T10:00:00 make=Apple model="iPhone 15 Pro" opt=1
  scn media.tiff media "Uncompressed TIFF with EXIF"
  img photos $m/media_tif.tif size=96x72 dt=2021-07-04T10:00:00
  scn media.heic media "HEIC with EXIF (skipped when heif-enc is not available)"
  img photos $m/media_heic.heic dt=2021-07-05T10:00:00 make=Apple model="iPhone 15 Pro" opt=1
  scn media.raw_cr3 media "Canon CR3 stand-in: JPEG content under a RAW extension (exiftool reports it as JPEG)"
  img photos $m/raw/IMG_2001.CR3 dt=2021-08-01T10:00:00 make=Canon model="Canon EOS R5"
  scn media.raw_nef media "Nikon NEF stand-in: TIFF content under a RAW extension"
  img photos $m/raw/DSC_2002.NEF size=96x72 dt=2021-08-02T10:00:00 make=NIKON model=D850
  scn media.raw_arw media "Sony ARW stand-in: TIFF content under a RAW extension"
  img photos $m/raw/DSC_2003.ARW size=96x72 dt=2021-08-03T10:00:00 make=Sony model=ILCE-7M4
  scn media.raw_dng media "Adobe DNG stand-in: TIFF content under a RAW extension"
  img photos $m/raw/IMG_2004.DNG size=96x72 dt=2021-08-04T10:00:00 make=Apple model="iPhone 15 Pro"
  scn media.video_mp4 media "MP4 with a UTC creation_time; the local capture time depends on the server time zone (manifest assumes TZ=UTC)"
  video photos $m/video/clip_mp4.mp4 mp4 opt=1
  scn media.video_mov media "QuickTime MOV with com.apple.quicktime.creationdate carrying a +01:00 offset (wins over the UTC creation_time)"
  video photos $m/video/clip_mov.mov mov opt=1
  scn media.video_nometa media "MP4 without any metadata: capture falls back to the file time"
  video photos $m/video/clip_nometa.mp4 nometa opt=1 mtime=2022-03-03T03:03:03
  scn other.text media "Text file in a photo folder: indexed as kind other"
  put_text photos $m/misc/notes.txt "Plain text, not a photo."
  scn other.binary media "Binary file with an unknown extension: indexed as kind other"
  put_bytes photos $m/misc/data.bin '\x00\x01\x02\xfe\xff\x00'
  scn other.trailing_space_ext media "Name ends in 'jpg ' (trailing space): not an image extension, so kind other"
  if can_name 'end space.jpg '; then
    put_text photos "$m/misc/end space.jpg " "The extension is 'jpg ' with a trailing space."
  else
    skip_scn other.trailing_space_ext "the filesystem does not keep trailing spaces in names" photos "$m/misc/end space.jpg "
  fi
}

# Sidecars live in one folder. put_xmp runs before the owner so xmp=1 can fold the sidecar values into the owner's
# expectations (sidecar fields win field by field; an embedded date wins over a sidecar date, so never both).
_small_sidecars() {
  local d=sidecars
  scn sidecar.exact sidecar "Exact link IMG_0001.CR3.xmp: the owner has no metadata, the sidecar supplies date, rating, pick, keywords and camera"
  put_xmp photos $d/IMG_0001.CR3.xmp rating=4 pick=1 make=Canon model="Canon EOS R5" date=2022-05-03T09:30:00 kw=Travel,Italy hier='Travel|Italy|Rome'
  img photos $d/IMG_0001.CR3 tag=0 xmp=1
  sc_link photos $d/IMG_0001.CR3 $d/IMG_0001.CR3.xmp exact

  scn sidecar.stem_shared sidecar "Stem link: IMG_0002.xmp belongs to both IMG_0002.CR3 and IMG_0002.JPG (rating -1 = rejected)"
  put_xmp photos $d/IMG_0002.xmp rating=-1
  img photos $d/IMG_0002.CR3 dt=2022-06-01T10:00:00 make=Canon model="Canon EOS R5" xmp=1
  img photos $d/IMG_0002.JPG dt=2022-06-01T10:00:00 make=Canon model="Canon EOS R5" xmp=1
  sc_link photos $d/IMG_0002.CR3 $d/IMG_0002.xmp stem
  sc_link photos $d/IMG_0002.JPG $d/IMG_0002.xmp stem
  dupgroup sidecar_raw_jpg_pair 4 "RAW+JPEG pair: equal stems always match in mode 4 (incidental to the sidecar scenario)" incidental
  dupsrc sidecar_raw_jpg_pair photos $d/IMG_0002.CR3
  dupmem sidecar_raw_jpg_pair photos $d/IMG_0002.JPG 4

  scn sidecar.override sidecar "Sidecar rating 5 and Sony camera replace the embedded rating 3 and Nikon D850; the embedded date stays (the sidecar has none)"
  put_xmp photos $d/IMG_0010.JPG.xmp rating=5 make=Sony model=ILCE-7M4
  img photos $d/IMG_0010.JPG dt=2022-07-07T07:07:07 make=NIKON model=D850 rating=3 xmp=1
  sc_link photos $d/IMG_0010.JPG $d/IMG_0010.JPG.xmp exact

  scn sidecar.case_insensitive sidecar "Lower-case img_0003.xmp (element style XMP) links to IMG_0003.JPG, rating 2"
  put_xmp photos $d/img_0003.xmp rating=2 style=elem
  img photos $d/IMG_0003.JPG dt=2022-07-08T08:08:08 xmp=1
  sc_link photos $d/IMG_0003.JPG $d/img_0003.xmp stem

  scn sidecar.upper_ext sidecar "Upper-case extension IMG_0005.XMP links to IMG_0005.jpg, rating 1"
  put_xmp photos $d/IMG_0005.XMP rating=1
  img photos $d/IMG_0005.jpg dt=2022-07-09T09:09:09 xmp=1
  sc_link photos $d/IMG_0005.jpg $d/IMG_0005.XMP stem

  scn sidecar.aae sidecar "Apple .aae edit sidecar (exact link, linked but not parsed)"
  put_text photos $d/IMG_0006.jpg.aae '<?xml version="1.0" encoding="UTF-8"?><plist version="1.0"><dict/></plist>'
  img photos $d/IMG_0006.jpg dt=2022-07-10T10:10:10
  sc_link photos $d/IMG_0006.jpg $d/IMG_0006.jpg.aae exact

  scn sidecar.json_takeout sidecar "Google Takeout style takeout_0007.jpg.json (exact link, linked but not parsed)"
  put_text photos $d/takeout_0007.jpg.json '{"title":"takeout_0007.jpg","description":"","imageViews":"1"}'
  img photos $d/takeout_0007.jpg dt=2022-07-11T11:11:11
  sc_link photos $d/takeout_0007.jpg $d/takeout_0007.jpg.json exact

  scn sidecar.dop sidecar "DxO .dop sidecar IMG_0008.dng.dop (exact link, linked but not parsed)"
  put_text photos $d/IMG_0008.dng.dop 'DxO PhotoLab sidecar placeholder'
  img photos $d/IMG_0008.dng size=96x72 dt=2022-07-12T12:12:12
  sc_link photos $d/IMG_0008.dng $d/IMG_0008.dng.dop exact

  scn sidecar.pp3 sidecar "RawTherapee .pp3 sidecar IMG_0009.pp3 (stem link, linked but not parsed)"
  put_text photos $d/IMG_0009.pp3 '[Version]
AppVersion=5.8'
  img photos $d/IMG_0009.NEF size=96x72 dt=2022-07-13T13:13:13
  sc_link photos $d/IMG_0009.NEF $d/IMG_0009.pp3 stem

  scn sidecar.xml sidecar "Camera .xml sidecar IMG_0011.xml (stem link, linked but not parsed)"
  put_text photos $d/IMG_0011.xml '<?xml version="1.0"?><NonRealTimeMeta/>'
  img photos $d/IMG_0011.jpg dt=2022-07-14T14:14:14
  sc_link photos $d/IMG_0011.jpg $d/IMG_0011.xml stem

  scn sidecar.txt_not_sidecar sidecar "A .txt next to a photo is not a sidecar: kind other, no link"
  put_text photos $d/IMG_0012.txt 'notes about the photo'
  img photos $d/IMG_0012.jpg dt=2022-07-15T15:15:15

  scn sidecar.orphan sidecar "Sidecar without an owner: kind sidecar, no link"
  put_xmp photos $d/orphan_0013.xmp rating=3

  scn sidecar.prefix_trap sidecar "IMG_0004.xmp must not link to IMG_00041.jpg although its stem starts with the sidecar stem"
  put_xmp photos $d/IMG_0004.xmp rating=2
  img photos $d/IMG_00041.jpg dt=2022-07-16T16:16:16

  scn sidecar.other_dir_orphan sidecar "A sidecar in a subfolder does not link to the same-named photo in the parent folder"
  put_xmp photos $d/elsewhere/IMG_0003.xmp rating=4
}

# Duplicate groups. Modes: 1 hash, 2 same name and capture second, 3 same name and different capture, 4 name prefix.
# Equal stems always match in mode 4, so mode 2 and 3 groups list "2,4" and "3,4".
_small_dups() {
  local src
  scn dup.m1_pair dup "Mode 1: byte-identical files with different names, one per volume"
  img photos dupes/m1/IMG_0300.jpg dt=2021-04-04T04:04:04 make=Canon model="Canon EOS R5"
  src=$FX_KEY
  put "$src" archive m1/vacation_copy.jpg
  dupgroup m1_pair 1 "Mode 1 pair across volumes"
  dupsrc m1_pair photos dupes/m1/IMG_0300.jpg
  dupmem m1_pair archive m1/vacation_copy.jpg 1

  scn dup.m1_triple dup "Mode 1: three identical files, one in a nested folder and one on the other volume"
  img photos dupes/m1/triple_a.jpg dt=2021-04-05T04:04:04
  src=$FX_KEY
  put "$src" photos dupes/m1/nested/holiday_b.jpg
  put "$src" archive "m1/archive scan_c.jpg"
  dupgroup m1_triple 1 "Mode 1 triple"
  dupsrc m1_triple photos dupes/m1/triple_a.jpg
  dupmem m1_triple photos dupes/m1/nested/holiday_b.jpg 1
  dupmem m1_triple archive "m1/archive scan_c.jpg" 1

  scn dup.m1_mtime dup "Mode 1: identical bytes with mtimes nine years apart (keep-oldest and keep-newest rules)"
  img photos dupes/m1/twin_old.jpg dt=2021-04-06T04:04:04 mtime=2015-03-03T10:00:00
  src=$FX_KEY
  put "$src" photos dupes/m1/twin_new.jpg mtime=2024-09-09T10:00:00
  dupgroup m1_mtime 1 "Mode 1 pair with different mtimes"
  dupsrc m1_mtime photos dupes/m1/twin_old.jpg
  dupmem m1_mtime photos dupes/m1/twin_new.jpg 1

  scn dup.m2_same_name_time dup "Mode 2: same file name and capture second, different pixels, same volume"
  img photos dupes/m2/a/Beach.jpg dt=2022-03-03T03:03:03 make=Sony model=ILCE-7M4
  img photos dupes/m2/b/Beach.jpg dt=2022-03-03T03:03:03 make=Sony model=ILCE-7M4
  dupgroup m2_same_name_time 2 "Mode 2 pair, same volume"
  dupsrc m2_same_name_time photos dupes/m2/a/Beach.jpg
  dupmem m2_same_name_time photos dupes/m2/b/Beach.jpg 2,4

  scn dup.m2_diff_dimensions dup "Mode 2: a resized copy (different dimensions and bytes) on the other volume"
  img photos dupes/m2/Mountain.jpg size=160x120 dt=2022-03-04T03:03:03
  img archive m2/Mountain.jpg size=200x150 dt=2022-03-04T03:03:03
  dupgroup m2_diff_dimensions 2 "Mode 2 pair with different dimensions"
  dupsrc m2_diff_dimensions photos dupes/m2/Mountain.jpg
  dupmem m2_diff_dimensions archive m2/Mountain.jpg 2,4

  scn dup.m3_diff_time dup "Mode 3: same file name, capture times differ by about nine hours"
  img photos dupes/m3/a/Sunset.jpg dt=2021-06-21T20:30:00
  img photos dupes/m3/b/Sunset.jpg dt=2021-06-22T05:10:00
  dupgroup m3_diff_time 3 "Mode 3 pair, same volume"
  dupsrc m3_diff_time photos dupes/m3/a/Sunset.jpg
  dupmem m3_diff_time photos dupes/m3/b/Sunset.jpg 3,4

  scn dup.m3_cross_volume dup "Mode 3: same file name and different capture times on two volumes"
  img photos dupes/m3/Lonely.jpg dt=2020-02-02T02:02:02
  img archive inbox/Lonely.jpg dt=2020-02-03T02:02:02
  dupgroup m3_cross_volume 3 "Mode 3 pair across volumes"
  dupsrc m3_cross_volume photos dupes/m3/Lonely.jpg
  dupmem m3_cross_volume archive inbox/Lonely.jpg 3,4

  scn dup.m4_prefix dup "Mode 4: names that extend the source stem. _edit, -1, ' (1)' and a PNG with the same stem match; IMG_04001 and IMG_0400a only match without the boundary rule (strict_prefix); IMG_00400 never matches"
  img photos dupes/m4/IMG_0400.jpg dt=2022-04-04T04:04:04
  img photos dupes/m4/IMG_0400_edit.jpg dt=2022-04-04T04:04:04
  img photos dupes/m4/IMG_0400-1.jpg dt=2022-04-04T04:04:04
  img photos "dupes/m4/IMG_0400 (1).jpg" dt=2022-04-04T04:04:04
  img photos dupes/m4/IMG_0400.png dt=2022-04-04T04:04:04
  img photos dupes/m4/IMG_04001.jpg dt=2022-04-04T04:04:04
  img photos dupes/m4/IMG_0400a.jpg dt=2022-04-04T04:04:04
  img photos dupes/m4/IMG_00400.jpg dt=2022-04-04T04:04:04
  dupgroup m4_prefix 4 "Mode 4 prefix family around IMG_0400"
  dupsrc m4_prefix photos dupes/m4/IMG_0400.jpg
  dupmem m4_prefix photos dupes/m4/IMG_0400_edit.jpg 4
  dupmem m4_prefix photos dupes/m4/IMG_0400-1.jpg 4
  dupmem m4_prefix photos "dupes/m4/IMG_0400 (1).jpg" 4
  dupmem m4_prefix photos dupes/m4/IMG_0400.png 4
  dupmem m4_prefix photos dupes/m4/IMG_04001.jpg - plain
  dupmem m4_prefix photos dupes/m4/IMG_0400a.jpg - plain
  dupmem m4_prefix photos dupes/m4/IMG_00400.jpg -

  scn dup.multi_mode dup "One source matching in several modes: identical copy (1,2,4), _edit (4) and a copy with another capture time (3,4)"
  img photos dupes/multi/IMG_0500.jpg dt=2022-05-05T05:05:05 make=Canon model="Canon EOS R5"
  src=$FX_KEY
  put "$src" archive multi/IMG_0500.jpg
  img photos dupes/multi/IMG_0500_edit.jpg dt=2022-05-05T05:05:05 make=Canon model="Canon EOS R5"
  img photos dupes/multi/other/IMG_0500.jpg dt=2022-05-06T05:05:05 make=Canon model="Canon EOS R5"
  dupgroup multi_mode multi "One source, three kinds of match"
  dupsrc multi_mode photos dupes/multi/IMG_0500.jpg
  dupmem multi_mode archive multi/IMG_0500.jpg 1,2,4
  dupmem multi_mode photos dupes/multi/IMG_0500_edit.jpg 4
  dupmem multi_mode photos dupes/multi/other/IMG_0500.jpg 3,4

  scn dup.unique_controls dup "Files that must never be reported as duplicates of anything"
  img photos dupes/unique/Zebra_001.jpg dt=2022-06-01T06:00:00
  img photos dupes/unique/Yak_002.jpg dt=2022-06-02T06:00:00
  img photos dupes/unique/Xerus_003.jpg dt=2022-06-03T06:00:00
  img archive 2019/ARC_0001.jpg dt=2019-01-01T06:00:00
  img archive inbox/ARC_0002.jpg dt=2019-01-02T06:00:00
  dupgroup unique_controls none "Controls without any duplicate"
  dupmem unique_controls photos dupes/unique/Zebra_001.jpg -
  dupmem unique_controls photos dupes/unique/Yak_002.jpg -
  dupmem unique_controls photos dupes/unique/Xerus_003.jpg -
  dupmem unique_controls archive 2019/ARC_0001.jpg -
  dupmem unique_controls archive inbox/ARC_0002.jpg -
}

_small_names() {
  local n=names nfc nfd emoji long
  scn names.spaces names "Spaces in folder and file names"
  img photos "$n/with spaces/sunny day.jpg" dt=2021-12-01T12:00:00
  nfc=$'\xc3\xa9t\xc3\xa9'
  nfd=$'e\xcc\x81te\xcc\x81'
  scn names.unicode_nfc names "Accented name stored in NFC (precomposed)"
  scn names.unicode_nfd names "The same accented name stored in NFD (decomposed): a different byte sequence, one logical name"
  if can_name "$nfc.jpg" && can_name "$nfd.jpg"; then
    SCN_KEY=names.unicode_nfc
    img photos "$n/nfc/$nfc.jpg" dt=2021-12-02T12:00:00
    SCN_KEY=names.unicode_nfd
    img photos "$n/nfd/$nfd.jpg" dt=2021-12-02T12:00:00
    dupgroup names_nfc_nfd 2 "Mode 2: the NFC and NFD spellings are the same normalised name"
    dupsrc names_nfc_nfd photos "$n/nfc/$nfc.jpg"
    dupmem names_nfc_nfd photos "$n/nfd/$nfd.jpg" 2,4
  else
    skip_scn names.unicode_nfc "the filesystem does not keep NFC and NFD names apart" photos "$n/nfc/$nfc.jpg"
    skip_scn names.unicode_nfd "the filesystem does not keep NFC and NFD names apart" photos "$n/nfd/$nfd.jpg"
  fi
  scn names.case_upper names "Upper-case extension and capital letter (Casing.JPG)"
  img photos $n/case_upper/Casing.JPG dt=2021-12-03T12:00:00
  scn names.case_lower names "Lower-case twin (casing.jpg): same normalised name and capture second as Casing.JPG"
  img photos $n/case_lower/casing.jpg dt=2021-12-03T12:00:00
  dupgroup names_case 2 "Mode 2: names differ only by letter case"
  dupsrc names_case photos $n/case_upper/Casing.JPG
  dupmem names_case photos $n/case_lower/casing.jpg 2,4
  emoji=$'\xf0\x9f\x93\xb7'
  scn names.emoji names "Emoji in the file name"
  img photos "$n/emoji/$emoji trip.jpg" dt=2021-12-04T12:00:00
  printf -v long '%*s' 191 ''
  long="long_${long// /n}.jpg"
  scn names.long_name names "File name of 200 characters"
  img photos "$n/long/$long" dt=2021-12-05T12:00:00
  scn names.leading_dot names "Name starting with a dot (.hidden.jpg) is an ordinary image, not junk (assumption, see README)"
  img photos $n/.hidden.jpg dt=2021-12-06T12:00:00
  scn names.trailing_space names "Space before the extension (stem ends with a space)"
  img photos "$n/Trailing space .jpg" dt=2021-12-07T12:00:00
  scn names.hash names "Hash sign in the name"
  img photos 'names/hash#1.jpg' dt=2021-12-08T12:00:00
  scn names.percent names "Percent sign in the name (looks like a URL escape)"
  img photos 'names/percent%20test.jpg' dt=2021-12-09T12:00:00
  scn names.apostrophe names "Apostrophe in the name"
  img photos "names/it's a photo.jpg" dt=2021-12-10T12:00:00
  scn names.punctuation names "Brackets and shell-special punctuation in the name"
  img photos 'names/misc[1](2)$x!+=;,@~&.jpg' dt=2021-12-11T12:00:00
}

_small_junk() {
  local j=junk
  scn junk.real_neighbour junk "Real photo next to the junk, so the folder is not empty"
  img photos $j/junk_real.jpg dt=2021-09-01T09:00:00
  dir_scn photos $j
  scn junk.eadir junk "Synology @eaDir thumbnail folder: skipped, never descended"
  put_junk photos "$j/@eaDir/IMG_0001.jpg/SYNOPHOTO_THUMB_M.jpg" '\xff\xd8\xff\xe0\x00\x10JFIF\x00\x01'
  scn junk.ds_store junk "macOS .DS_Store"
  put_junk photos $j/.DS_Store '\x00\x00\x00\x01Bud1'
  scn junk.thumbs_db junk "Windows Thumbs.db"
  put_junk photos $j/Thumbs.db '\xd0\xcf\x11\xe0\xa1\xb1\x1a\xe1'
  scn junk.desktop_ini junk "Windows desktop.ini"
  put_junk photos $j/desktop.ini '[.ShellClassInfo]\r\nIconFile=x.ico\r\n'
  scn junk.appledouble junk "AppleDouble resource fork file (._IMG_0001.jpg)"
  put_junk photos $j/._IMG_0001.jpg '\x00\x05\x16\x07\x00\x02\x00\x00'
  scn junk.recycle junk "Synology #recycle folder with a photo inside"
  put_junk photos "$j/#recycle/x.jpg" '\xff\xd8\xff\xd9'
  scn junk.snapshot junk "Synology #snapshot folder with a photo inside"
  put_junk photos "$j/#snapshot/y.jpg" '\xff\xd8\xff\xd9'
  scn junk.apm_trash junk "The app's own .apm-trash folder"
  put_junk photos $j/.apm-trash/old/z.jpg '\xff\xd8\xff\xd9'
  scn junk.apm_part junk "Partial upload (.apm-part)"
  put_junk photos $j/upload.jpg.apm-part 'partial upload'
  scn junk.volume_root junk "Junk folders directly under the archive volume root"
  put_junk archive '#recycle/deleted.jpg' '\xff\xd8\xff\xd9'
  put_junk archive '@eaDir/ARC.jpg/SYNOPHOTO_THUMB_S.jpg' '\xff\xd8\xff\xd9'
  put_junk archive .apm-trash/old.jpg '\xff\xd8\xff\xd9'
}

# Empty means "no indexable file anywhere below" (junk does not count), so cleanup tests can predict what to remove.
_small_empty() {
  local e=empty
  scn empty.nested_tree empty "Nested folders without any file: every level is empty"
  empty_dir photos $e/nested/a/b/c
  dir_scn photos $e/nested
  scn empty.junk_only empty "Folder holding only junk files (.DS_Store, Thumbs.db): empty for the index"
  put_junk photos $e/junk_only/.DS_Store '\x00\x00\x00\x01Bud1'
  put_junk photos $e/junk_only/Thumbs.db '\xd0\xcf\x11\xe0\xa1\xb1\x1a\xe1'
  dir_scn photos $e/junk_only
  scn empty.eadir_only empty "Folder whose only content is an @eaDir folder: empty for the index"
  empty_dir photos "$e/eadir_only/@eaDir"
  dir_scn photos $e/eadir_only
  scn empty.chain empty "Chain of folders ending in a junk file: the whole chain is empty"
  put_junk photos $e/chain/one/two/three/.DS_Store '\x00\x00\x00\x01Bud1'
  dir_scn photos $e/chain
  scn empty.mixed empty "Folder with a real photo and an empty subfolder: only the subfolder is empty"
  img photos $e/mixed/keep.jpg dt=2021-09-02T09:00:00
  empty_dir photos $e/mixed/hollow
  dir_scn photos $e/mixed
  scn empty.cascade empty "Folder whose only children are an empty folder and a junk-only folder: empty once they are removed"
  empty_dir photos $e/cascade/child1
  put_junk photos $e/cascade/child2/.DS_Store '\x00\x00\x00\x01Bud1'
  dir_scn photos $e/cascade
}

_small_symlinks() {
  local l=links
  scn symlink.control symlink "Real photo next to the symlinks (control)"
  img photos $l/links_real.jpg dt=2021-10-01T10:00:00
  scn symlink.escape_dir symlink "Symlink to a directory outside the volume (../../../outside)"
  mk_symlink photos $l/escape_dir ../../../outside
  scn symlink.escape_file symlink "Symlink to a file outside the volume"
  mk_symlink photos $l/escape_file.jpg ../../../outside/secret.jpg
  scn symlink.inside_file symlink "Symlink to a file inside the same volume: still skipped"
  mk_symlink photos $l/inside_file.jpg ../2023/06/IMG_1103.jpg
  scn symlink.inside_dir symlink "Symlink to a directory inside the same volume: still skipped"
  mk_symlink photos $l/inside_dir ../2024
  scn symlink.loop symlink "Two symlinks pointing at each other"
  mk_symlink photos $l/loop_a loop_b
  mk_symlink photos $l/loop_b loop_a
  scn symlink.parent symlink "Symlink to the parent folder (endless recursion when followed)"
  mk_symlink photos $l/up ..
  scn symlink.dangling symlink "Symlink to a file that does not exist"
  mk_symlink photos $l/dangling.jpg ../does_not_exist.jpg
  dir_scn photos $l symlink.control
  mkdir -p "$OUT/outside"
  _jpeg 999 16x12 "$OUT/outside/secret.jpg" 0 || fx_die "cannot create outside/secret.jpg"
}

_small_deep() {
  local p=deep i n dt
  scn deep.path_12 deep "Photo twelve folders deep"
  for i in 01 02 03 04 05 06 07 08 09 10 11 12; do p="$p/l$i"; done
  img photos "$p/deep_photo.jpg" dt=2021-11-01T10:00:00
  dir_scn photos deep
  scn deep.grid_500 deep "One folder with $GRID_FILES tiny images (grid and pagination tests); one mtime for all of them"
  for ((i = 1; i <= GRID_FILES; i++)); do
    fx_iso_from_epoch $((1704067200 + (i - 1) * 10800))
    dt=$FX_R
    printf -v n 'GRID_%04d.jpg' "$i"
    put_dated photos "grid/$n" $((i % 4)) $((i % 8)) "$dt" mtime=2024-01-01T00:00:00
  done
  dir_scn photos grid
  scn deep.only_subfolders deep "Folder that holds only subfolders and no file of its own"
  img photos only_subfolders/a/sub_a_photo.jpg dt=2021-11-02T10:00:00
  img photos only_subfolders/b/sub_b_photo.jpg dt=2021-11-03T10:00:00
  dir_scn photos only_subfolders
}

# Baseline for mutations.sh (see README): the script renames, edits, touches, removes and adds files below stale/.
_small_stale() {
  local s=stale
  scn stale.rename stale "mutations.sh renames rename_me.jpg to renamed.jpg"
  img photos $s/rename_me.jpg dt=2021-01-10T10:00:00
  scn stale.bytes_changed stale "mutations.sh flips one byte of the JPEG header; size and mtime stay the same"
  img photos $s/bytes_changed.jpg dt=2021-01-11T10:00:00
  scn stale.mtime_only stale "mutations.sh changes only the mtime"
  img photos $s/mtime_only.jpg dt=2021-01-12T10:00:00
  scn stale.delete_dir stale "mutations.sh removes doomed_dir including its subfolder"
  img photos $s/doomed_dir/gone_1.jpg dt=2021-01-13T10:00:00
  img photos $s/doomed_dir/gone_2.jpg dt=2021-01-14T10:00:00
  img photos $s/doomed_dir/sub/gone_3.jpg dt=2021-01-15T10:00:00
  dir_scn photos $s/doomed_dir
  scn stale.new_sidecar stale "mutations.sh adds has_new_sidecar.xmp next to has_new_sidecar.jpg"
  img photos $s/has_new_sidecar.jpg dt=2021-01-16T10:00:00
  scn stale.touch_xmp stale "mutations.sh rewrites touched.xmp (rating 2 becomes 3) and gives it a new mtime"
  put_xmp photos $s/touched.xmp rating=2
  img photos $s/touched.jpg dt=2021-01-17T10:00:00 xmp=1
  sc_link photos $s/touched.jpg $s/touched.xmp stem
}

# Permission scenarios; generate.sh applies chmod 000 as its very last step. They do nothing when run as root.
_small_perms() {
  scn perms.unreadable_file perms "File with mode 000: still indexed by the scanner, content unreadable (no effect when running as root)"
  img photos perms/unreadable.jpg tag=0 unread=file mtime=2021-12-20T10:00:00
  scn perms.unreadable_dir perms "Folder with mode 000: cannot be listed, so its content is not indexable (no effect when running as root)"
  img photos perms/locked_dir/inside.jpg tag=0 unread=dir mtime=2021-12-21T10:00:00
}
