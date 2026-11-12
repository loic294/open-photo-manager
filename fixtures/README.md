# Fixture photo library

A deterministic generator for a realistic photo library on disk. The backend integration tests, the Playwright suite and
the load tests all use it, and `manifest.json` says what the indexer is expected to find in every file. Only the
generator is committed; the output goes to `fixtures/out` (git-ignored).

```sh
make fixtures          # small catalogue: 638 files, 2.7 MB; about 9 s the first time, 0.3 s when up to date
make fixtures-verify   # re-read the tree and compare it with manifest.json
make fixtures-large    # 200000 tiny files in fixtures/out-large; about 1 minute
make fixtures-clean    # remove both output directories
```

Point the app at `fixtures/out/volumes` (`OPM_VOLUMES_DIR`, the root Makefile default). Every folder in it is a volume:
`photos` holds the whole catalogue and `archive` is the second volume for cross-volume tests.

1. [Requirements](#requirements)
2. [Make targets](#make-targets)
3. [generate.sh](#generatesh)
4. [Output layout](#output-layout)
5. [Scenario catalogue (small scale)](#scenario-catalogue-small-scale)
6. [Large scale](#large-scale)
7. [manifest.json](#manifestjson)
8. [Flat TSV files](#flat-tsv-files)
9. [What the expected values mean](#what-the-expected-values-mean)
10. [Using the fixtures in tests](#using-the-fixtures-in-tests)
11. [verify.sh](#verifysh)
12. [mutations.sh](#mutationssh)
13. [Assumptions and limits](#assumptions-and-limits)
14. [Extending the catalogue](#extending-the-catalogue)

## Requirements

`bash` 3.2 or newer (the macOS default works), `exiftool`, `ffmpeg` (with libx264), `vips`, and the usual `sha1sum` (or
`shasum`), `awk`, `od`, `find`, `xargs` and `sort`. `heif-enc` (libheif) is optional: without it the HEIC scenario is
skipped with a warning and recorded as skipped in the manifest. Nothing else is needed (no jq, python or perl), so the
scripts also run inside the Debian runtime image. The tools are checked up front and the error message carries the
install hints:

```sh
apt-get install libimage-exiftool-perl ffmpeg libvips-tools libheif-examples   # Debian, Ubuntu
brew install exiftool ffmpeg vips libheif                                       # macOS
```

Tested on macOS (bash 3.2, exiftool 13.33, ffmpeg 9.0, vips 8.18, libheif 1.23) and on `debian:bookworm-slim` (bash 5.2,
mawk, exiftool 12.57, ffmpeg 5.1, vips 8.14), as a normal user and as root, with and without `heif-enc`.

## Make targets

`fixtures/fixtures.mk` is included by the root Makefile.

| Target | Runs | Result |
|---|---|---|
| `make fixtures` | `generate.sh --out $(FIXTURES_OUT)` | small catalogue (default `fixtures/out`) |
| `make fixtures-large` | `generate.sh --scale large --files $(FIXTURES_FILES) --out $(FIXTURES_LARGE_OUT)` | bulk tree (default `fixtures/out-large`, 200000 files) |
| `make fixtures-verify` | `verify.sh --out $(FIXTURES_OUT)` | checks the small tree |
| `make fixtures-clean` | `generate.sh --clean-only` on both directories | removes them |

Variables: `FIXTURES_OUT`, `FIXTURES_LARGE_OUT`, `FIXTURES_FILES`, for example
`make fixtures-large FIXTURES_FILES=20000`. The Docker build context must exclude `fixtures/out*` (the large tree is
`fixtures/out-large`, which a plain `fixtures/out` entry does not cover).

## generate.sh

```
fixtures/generate.sh [--out DIR] [--scale small|large] [--files N] [--clean] [--clean-only] [--jobs N] [--no-exhaustive]
```

| Option | Meaning |
|---|---|
| `--out DIR` | output directory, default `fixtures/out`; parents are created |
| `--scale small` | the scenario catalogue (default) |
| `--scale large` | tiny generated files for load tests |
| `--files N` | file count at large scale (default 200000, minimum 1000) |
| `--clean` | regenerate even when the output is up to date |
| `--clean-only` | remove the output directory and stop |
| `--jobs N` | parallel file writers at large scale (default CPU count, at most 8) |
| `--no-exhaustive` | skip the pairwise duplicate cross-check inside the manifest builder (faster at very large scale) |

- **Deterministic.** The same arguments and the same tool versions give the same names, bytes and mtimes (mtimes are
  fixed and written in UTC; the machine's time zone does not matter). The bytes of the encoded media (JPEG, PNG, WebP,
  HEIC, video) do depend on the tool versions, so a sha1 is only stable on one toolset. Never hard-code a hash or a size
  in a test; read it from the manifest. The versions are recorded in `generator.tools`.
- **Idempotent.** `OUT/.fixtures-stamp` stores a signature (sha1 of the generator sources, the arguments and the tool
  versions) and a fingerprint of the tree (names, sizes, mtimes, link targets). When both still match, the run prints
  `up to date` and changes nothing (0.3 s). An edited generator, a tool upgrade, or a tree whose folders, files, sizes
  or mtimes differ from the stamp (something added, removed, renamed or touched, or a `mutations.sh` run) leads to a
  full rebuild on the next call.
- **Safe.** A directory is only replaced when it carries the `.fixtures-owned` marker written by this tool, so a typo in
  `--out` cannot erase somebody's folder. Concurrent runs for the same `--out` wait on a lock directory (`OUT.lock`;
  a dead owner's lock is removed), so parallel test jobs may all call `make fixtures`.
- **Clean up with `--clean-only`, not `rm -rf`.** The `perms` scenario creates a folder and a file with mode 000, and the
  generator restores the modes first. If a plain `rm -rf` already failed on them, `--clean-only` refuses (the marker is
  gone); recover with `chmod -R u+rwX OUT && rm -rf OUT`.
- **Loud failures.** The scripts stop at the first problem and print the failing command and line.

## Output layout

```
OUT/
  volumes/
    photos/            main volume; the whole catalogue lives here
    archive/           second volume: cross-volume duplicates, junk at a volume root
  outside/secret.jpg   target of the symlinks that leave the volume (small scale only; not inside a volume)
  manifest.json        expected result for every file (schema below)
  manifest.tsv         one line per file, the flat version of manifest.json
  manifest.dirs.tsv    folders and whether they are empty for the index
  manifest.dups.tsv    duplicate groups, one line per member
  README.txt           the scenarios of this very tree, and what was skipped
  .fixtures-owned  .fixtures-stamp  .fixtures-unreadable   bookkeeping of the generator
```

`mutations.sh` adds `.mutations-applied` and `mutations.tsv`. The scratch folder `OUT/.work` exists only while
generating.

## Scenario catalogue (small scale)

Every scenario has a stable key `group.name`, which is the `scenario` field of its files in the manifest. The keys are
the contract with the tests: they are not renamed, and new scenarios only add keys. `manifest.json` under `scenarios`
(and `OUT/README.txt`) lists every key with its description. Small scale: 638 files and 8 symlinks, 106 scenarios,
13 duplicate groups, 91 indexable folders (14 empty).

Paths are relative to `volumes/photos` unless they start with `archive:`. Times are naive local, as the indexer stores
them (all fixtures assume `TZ=UTC`, see [Assumptions](#assumptions-and-limits)).

### exif: JPEG metadata variety

| Key | File | Expectation |
|---|---|---|
| `exif.full_nikon` | `2021/03/DSC_0001.jpg` | `NIKON D850`, 2021-03-14T09:26:53 from EXIF, with SubSecTimeOriginal 250 (the fraction is dropped, never rounded) |
| `exif.dec31_offset` | `2022/12/IMG_1101.jpg` | 2022-12-31T23:59:59, offset -300 min, `Canon EOS R5` (date-folder boundary) |
| `exif.jan1_offset` | `2023/01/IMG_1102.jpg` | 2023-01-01T00:00:00, offset +120 min; make `Canon`, model `EOS R5` becomes `Canon EOS R5` |
| `exif.iphone_subsec` | `2023/06/IMG_1103.jpg` | 2023-06-15T18:42:07 (SubSec 987 must not round up), offset +120, `Apple iPhone 15 Pro` |
| `exif.leap_day_sony` | `2024/02/IMG_1104.jpg` | 2024-02-29T12:00:00, `Sony ILCE-7M4` |
| `exif.make_prefix_nikon` | `2025/08/DSC_0002.jpg` | 2025-08-09T07:15:30; model `NIKON Z 7` already starts with the make: camera `NIKON Z 7` |
| `exif.make_case_canon` | `2026/01/IMG_1105.jpg` | 2026-01-02T20:00:00; make `CANON`, model `Canon EOS 5D Mark IV`: camera `Canon EOS 5D Mark IV` (case-insensitive prefix) |
| `exif.no_camera` | `no_camera/IMG_1106.jpg` | date 2020-05-05T05:05:05, camera null |
| `exif.no_date` | `no_date/IMG_1107.jpg` | camera `Canon EOS R5`, no date: source `fs` (file time; the mtime is 2019-07-04T10:00:00Z) |
| `exif.create_date_only` | `2023/07/IMG_1108.jpg` | only CreateDate 2023-07-07T07:07:07: source `exif` |
| `exif.rating_pick_keywords` | `2022/08/IMG_1109.jpg` | 2022-08-20T11:11:11; embedded XMP: rating 3, `selected`, tags Family, Holiday, Madrid, Spain, Travel (the hierarchy `Travel\|Spain\|Madrid` is flattened) |
| `exif.rating_rejected` | `2022/08/IMG_1110.jpg` | 2022-08-21T11:11:11; embedded rating -1: rating 0, flag `rejected` |
| `exif.gps_paris` | `places/IMG_1111.jpg` | 2023-09-09T12:00:00; location 48.8584, 2.2945 |
| `exif.gps_sydney` | `places/IMG_1112.jpg` | 2023-10-10T12:00:00; location -33.8688, 151.2093 |
| `exif.corrupt_header` | `broken/exif_bad_header.jpg` | IFD0 offset overwritten (exiftool: `Bad IFD0 directory`), pixels intact (160x120): no metadata readable, source `fs` |
| `exif.truncated` | `broken/exif_truncated.jpg` | 60 bytes, cut inside the EXIF segment (exiftool: `JPEG format error`): no metadata and no size |
| `exif.zero_byte` | `broken/zero_byte.jpg` | an empty file with a `.jpg` name: size 0, source `fs` |
| `exif.large_file` | `big/large_noise.jpg` | 1.5 MB of noise at 1920x1280, `Fujifilm X-T5` |

The three broken files must be indexed without any crash and with the file-time fallback.

### media: other formats

| Key | File | Expectation |
|---|---|---|
| `media.png` | `media/media_png.png` | EXIF in the PNG: 2021-07-01T10:00:00, `Canon EOS R5` |
| `media.gif` | `media/media_gif.gif` | GIF cannot carry EXIF: source `fs` |
| `media.webp` | `media/media_webp.webp` | 2021-07-03T10:00:00, `Apple iPhone 15 Pro` |
| `media.tiff` | `media/media_tif.tif` | 96x72 TIFF, 2021-07-04T10:00:00 |
| `media.heic` | `media/media_heic.heic` | 2021-07-05T10:00:00, `Apple iPhone 15 Pro`; skipped without `heif-enc` |
| `media.raw_cr3` | `media/raw/IMG_2001.CR3` | JPEG content (exiftool: JPEG), 2021-08-01T10:00:00, `Canon EOS R5` |
| `media.raw_nef` | `media/raw/DSC_2002.NEF` | TIFF content (exiftool: NEF), 2021-08-02T10:00:00, `NIKON D850` |
| `media.raw_arw` | `media/raw/DSC_2003.ARW` | TIFF content (exiftool: ARW), 2021-08-03T10:00:00, `Sony ILCE-7M4` |
| `media.raw_dng` | `media/raw/IMG_2004.DNG` | TIFF content (exiftool: DNG), 2021-08-04T10:00:00, `Apple iPhone 15 Pro` |
| `media.video_mp4` | `media/video/clip_mp4.mp4` | h264 96x64, 1 s, `creation_time` 2022-08-15T10:20:30Z; capture 2022-08-15T10:20:30 only under `TZ=UTC` (`tz_dependent`) |
| `media.video_mov` | `media/video/clip_mov.mov` | `com.apple.quicktime.creationdate` 2023-11-20T14:05:09+01:00 wins over the UTC `creation_time` 13:05:09: capture 2023-11-20T14:05:09, offset +60 |
| `media.video_nometa` | `media/video/clip_nometa.mp4` | no metadata: source `fs` (exiftool shows `CreateDate 0000:00:00 00:00:00`, which means absent) |
| `other.text` | `media/misc/notes.txt` | kind `other` |
| `other.binary` | `media/misc/data.bin` | kind `other` |
| `other.trailing_space_ext` | `media/misc/end space.jpg ` | the extension is `jpg ` (trailing space): kind `other` |

The RAW files are stand-ins: real RAW files cannot be synthesised with the tool list, so they are a JPEG (CR3) or an
uncompressed TIFF (NEF, ARW, DNG) with the right extension and complete EXIF. exiftool reads Make, Model and
DateTimeOriginal from all four; its `FileType` is JPEG for the CR3 stand-in and NEF, ARW, DNG for the others. Videos
are 96x64, 1 s, about 9 KB.

### sidecar: linking and merging (folder `sidecars/`)

| Key | Files | Expectation |
|---|---|---|
| `sidecar.exact` | `IMG_0001.CR3` + `IMG_0001.CR3.xmp` | exact link. The owner has no metadata; the XMP supplies capture 2022-05-03T09:30:00 (source `xmp`), rating 4, `selected`, tags Italy, Rome, Travel, camera `Canon EOS R5` |
| `sidecar.stem_shared` | `IMG_0002.CR3`, `IMG_0002.JPG`, `IMG_0002.xmp` | stem link to both owners; `xmp:Rating=-1` gives rating 0 and `rejected` |
| `sidecar.override` | `IMG_0010.JPG` + `IMG_0010.JPG.xmp` | sidecar rating 5 and camera `Sony ILCE-7M4` replace the embedded rating 3 and `NIKON D850`; the embedded date stays |
| `sidecar.case_insensitive` | `IMG_0003.JPG` + `img_0003.xmp` | stem link although the case differs; rating 2 |
| `sidecar.upper_ext` | `IMG_0005.jpg` + `IMG_0005.XMP` | upper-case extension; rating 1 |
| `sidecar.aae` | `IMG_0006.jpg` + `IMG_0006.jpg.aae` | exact link, not parsed |
| `sidecar.json_takeout` | `takeout_0007.jpg` + `takeout_0007.jpg.json` | exact link, not parsed |
| `sidecar.dop` | `IMG_0008.dng` + `IMG_0008.dng.dop` | exact link, not parsed |
| `sidecar.pp3` | `IMG_0009.NEF` + `IMG_0009.pp3` | stem link, not parsed |
| `sidecar.xml` | `IMG_0011.jpg` + `IMG_0011.xml` | stem link, not parsed |
| `sidecar.txt_not_sidecar` | `IMG_0012.jpg` + `IMG_0012.txt` | `.txt` is kind `other`, no link |
| `sidecar.orphan` | `orphan_0013.xmp` | kind `sidecar`, no owner |
| `sidecar.prefix_trap` | `IMG_0004.xmp`, `IMG_00041.jpg` | no link: the sidecar name is only a prefix of the photo name |
| `sidecar.other_dir_orphan` | `elsewhere/IMG_0003.xmp` | no link to `IMG_0003.JPG` in the parent folder |

### dup: one group per match mode

`photos/dupes/...` holds the sources, `archive:` the counterparts on the other volume. The modes are the ones of the
plan: 1 same hash, 2 same normalised name and same capture second, 3 same name and a different capture time, 4 name
prefix with a word boundary. The id of a group (in `duplicate_groups` and in the `groups` of a file) is the key without
the `dup.` prefix, for example `m1_pair`.

| Key | Files | Expectation |
|---|---|---|
| `dup.m1_pair` | `dupes/m1/IMG_0300.jpg`, `archive:m1/vacation_copy.jpg` | mode 1 across volumes, different names |
| `dup.m1_triple` | `dupes/m1/triple_a.jpg`, `dupes/m1/nested/holiday_b.jpg`, `archive:m1/archive scan_c.jpg` | mode 1, three members |
| `dup.m1_mtime` | `dupes/m1/twin_old.jpg`, `dupes/m1/twin_new.jpg` | identical bytes, mtimes 2015-03-03 and 2024-09-09 (keep-oldest and keep-newest rules) |
| `dup.m2_same_name_time` | `dupes/m2/a/Beach.jpg`, `dupes/m2/b/Beach.jpg` | mode 2: same name, same capture second, different pixels |
| `dup.m2_diff_dimensions` | `dupes/m2/Mountain.jpg`, `archive:m2/Mountain.jpg` | mode 2: a resized copy |
| `dup.m3_diff_time` | `dupes/m3/a/Sunset.jpg`, `dupes/m3/b/Sunset.jpg` | mode 3: same name, capture times about nine hours apart |
| `dup.m3_cross_volume` | `dupes/m3/Lonely.jpg`, `archive:inbox/Lonely.jpg` | mode 3: same name, capture times one day apart, on two volumes |
| `dup.m4_prefix` | `dupes/m4/IMG_0400.jpg` (source) and 7 more | `IMG_0400_edit`, `IMG_0400-1`, `IMG_0400 (1)` and `IMG_0400.png` match; `IMG_04001` and `IMG_0400a` match only without the word boundary (strict prefix); `IMG_00400` never |
| `dup.multi_mode` | `dupes/multi/IMG_0500.jpg` (source), `archive:multi/IMG_0500.jpg`, `dupes/multi/IMG_0500_edit.jpg`, `dupes/multi/other/IMG_0500.jpg` | the copy matches in modes 1, 2 and 4, the `_edit` in 4, the same-named file with another capture time in 3 and 4 |
| `dup.unique_controls` | `dupes/unique/{Zebra_001,Yak_002,Xerus_003}.jpg`, `archive:2019/ARC_0001.jpg`, `archive:inbox/ARC_0002.jpg` | never a duplicate of anything |

Three more groups belong to other scenarios: `names_nfc_nfd` and `names_case` (mode 2, see `names`) and
`sidecar_raw_jpg_pair` (mode 4, `incidental: true`: a RAW and a JPEG with the same stem).

### names: normalisation stress

| Key | File | Expectation |
|---|---|---|
| `names.spaces` | `names/with spaces/sunny day.jpg` | spaces in folder and file name |
| `names.unicode_nfc` | `names/nfc/été.jpg` | precomposed name |
| `names.unicode_nfd` | `names/nfd/été.jpg` | decomposed name (other bytes, same logical name); mode 2 duplicate of the NFC file |
| `names.case_upper` | `names/case_upper/Casing.JPG` | capital letters and an upper-case extension |
| `names.case_lower` | `names/case_lower/casing.jpg` | same normalised name and capture second: mode 2 duplicate of `Casing.JPG` |
| `names.emoji` | `names/emoji/📷 trip.jpg` | emoji in the name |
| `names.long_name` | `names/long/long_nnn...nnn.jpg` | 200 characters |
| `names.leading_dot` | `names/.hidden.jpg` | an ordinary image, not junk (assumption) |
| `names.trailing_space` | `names/Trailing space .jpg` | space before the extension |
| `names.hash` | `names/hash#1.jpg` | `#` in the name |
| `names.percent` | `names/percent%20test.jpg` | `%20` in the name (looks like a URL escape) |
| `names.apostrophe` | `names/it's a photo.jpg` | `'` in the name |
| `names.punctuation` | `names/misc[1](2)$x!+=;,@~&.jpg` | brackets and shell-special characters |

The NFC/NFD pair and `other.trailing_space_ext` probe the filesystem first and are skipped (recorded as skipped) when it
cannot store such names. The NFC and NFD names, and `Casing.JPG` and `casing.jpg`, sit in different folders so that
they also coexist on case-insensitive or normalising filesystems.

### junk, empty, symlink, deep, perms, stale

| Key | Path | Expectation |
|---|---|---|
| `junk.real_neighbour` | `junk/junk_real.jpg` | a real photo next to the junk, so the folder is not empty |
| `junk.eadir` | `junk/@eaDir/IMG_0001.jpg/SYNOPHOTO_THUMB_M.jpg` | skipped, folder never entered |
| `junk.ds_store`, `junk.thumbs_db`, `junk.desktop_ini` | `junk/.DS_Store`, `junk/Thumbs.db`, `junk/desktop.ini` | skipped |
| `junk.appledouble` | `junk/._IMG_0001.jpg` | skipped |
| `junk.recycle`, `junk.snapshot` | `junk/#recycle/x.jpg`, `junk/#snapshot/y.jpg` | skipped, folders never entered |
| `junk.opm_trash`, `junk.opm_part` | `junk/.opm-trash/old/z.jpg`, `junk/upload.jpg.opm-part` | skipped |
| `junk.volume_root` | `archive:#recycle/deleted.jpg`, `archive:.opm-trash/old.jpg`, `archive:@eaDir/ARC.jpg/SYNOPHOTO_THUMB_S.jpg` | junk directly under a volume root |
| `empty.nested_tree` | `empty/nested/a/b/c` | every level empty |
| `empty.junk_only` | `empty/junk_only` (`.DS_Store`, `Thumbs.db`) | empty for the index |
| `empty.eadir_only` | `empty/eadir_only` (only an `@eaDir`) | empty for the index |
| `empty.chain` | `empty/chain/one/two/three` (junk at the deepest level) | the whole chain is empty |
| `empty.mixed` | `empty/mixed/keep.jpg` and `empty/mixed/hollow` | only `hollow` is empty |
| `empty.cascade` | `empty/cascade/child1` (empty), `child2` (junk only) | all three folders are empty |
| `symlink.control` | `links/links_real.jpg` | a real photo next to the links |
| `symlink.escape_dir`, `symlink.escape_file` | `links/escape_dir`, `links/escape_file.jpg` | point outside the volume (`outside/`); skipped |
| `symlink.inside_file`, `symlink.inside_dir` | `links/inside_file.jpg`, `links/inside_dir` | point inside the volume; still skipped |
| `symlink.loop`, `symlink.parent`, `symlink.dangling` | `links/loop_a`, `links/loop_b`, `links/up`, `links/dangling.jpg` | loop, parent folder and missing target; skipped |
| `deep.path_12` | `deep/l01/.../l12/deep_photo.jpg` | twelve folders deep |
| `deep.grid_500` | `grid/GRID_0001.jpg` to `GRID_0500.jpg` | 500 tiny (16x12) images in one folder, one mtime (2024-01-01T00:00:00Z) for all, capture times three hours apart, four cameras in rotation |
| `deep.only_subfolders` | `only_subfolders/a/sub_a_photo.jpg`, `only_subfolders/b/sub_b_photo.jpg` | the folder holds subfolders only and no file of its own |
| `perms.unreadable_file` | `perms/unreadable.jpg` | mode 000: listed by the scanner, content unreadable (`unreadable: "file"`) |
| `perms.unreadable_dir` | `perms/locked_dir/inside.jpg` | mode 000 folder: cannot be listed, so the file is not indexable (`unreadable: "dir"`) |
| `stale.rename`, `stale.bytes_changed`, `stale.mtime_only`, `stale.delete_dir`, `stale.new_sidecar`, `stale.touch_xmp` | `stale/rename_me.jpg`, `bytes_changed.jpg`, `mtime_only.jpg`, `doomed_dir/...`, `has_new_sidecar.jpg`, `touched.jpg` + `touched.xmp` | the targets of [mutations.sh](#mutationssh); unchanged in the baseline |

The permission scenarios have no effect when the generator or the tests run as root.

## Large scale

`generate.sh --scale large --files N` (default 200000, minimum 1000) writes about N tiny files for load tests: N is the
total including copies and sidecars, and the generator takes the largest number of unique photos that fits. The layout
follows fixed formulas, so two runs with the same N are identical. With `n` the number in the file name
(`IMG_000001`, `IMG_000002`, ...):

- Photos: 150 per folder in `photos/bulk/YYYY/YYYY-MM-DD_gNNNNN/IMG_NNNNNN.jpg` (the folder date is that of its first
  photo). 16x12 JPEGs with a unique EXIF date, spread evenly over ten years from 2015-01-01, and one of four cameras in
  rotation. All mtimes are 2020-01-01T12:00:00Z. Key `bulk.unique`.
- Mode 1: photos with `n % 20 == 1` have a byte-identical copy `archive/bulk1/aNNNN/COPY_NNNNNN.jpg` (key `bulk.dup_hash`).
- Mode 2: photos with `n % 20 == 8` have a copy with the same file name and capture second but different pixels in
  `archive/bulk2/aNNNN/IMG_NNNNNN.jpg` (key `bulk.dup_name_time`).
- Sidecars: photos with `n % 50 == 12` have an `.xmp` with rating 1 to 5 next to them, alternately linked by stem
  (`IMG_000012.xmp`) and by exact name (`IMG_000062.jpg.xmp`) (key `bulk.sidecar`).
- The percentages are relative to the unique photos: 5 % mode 1, 5 % mode 2 and 2 % sidecars. For N=200000 that is 178570
  unique photos, 8929 + 8929 copies and 3572 sidecars.

There is no `outside/` folder and there are no symlinks, junk, empty folders or mode 3 and 4 duplicates at this scale.
The files are not written by one exiftool run per file: 32 pre-tagged JPEG templates (4 cameras x 8 pixel variants) are
patched with the unique date, so the generator scales linearly with no process per file. `verify.sh` re-reads a sample
with exiftool to prove that the patched files carry the dates the manifest claims.

| Files | Generate | `verify.sh` | Notes |
|---|---|---|---|
| 3000 | 2.4 s | 6 s | hashes and re-reads everything and regenerates |
| 20000 | 6 s | 30 s | same |
| 200000 | 64 s | 19 s with `--no-regen` | 151 MB of data (about 1 GB on disk with 4 KB blocks); `manifest.json` 141 MB, `manifest.tsv` 38 MB |

(Measured on a 10-core Apple laptop.) For load tests read `manifest.tsv`, not the JSON.

## manifest.json

Written from what is actually on disk (sizes, mtimes and sha1 are measured after generation) and cross-checked against
the intended expectations, so a mismatch fails the generation instead of producing a wrong manifest. The `files` array
has one object per line, so `grep` and line-oriented tools work on it.

Top-level keys:

| Key | Content |
|---|---|
| `schema_version` | `1` |
| `generator` | `scale`, `entries`, `signature`, `tz_assumed` (`"UTC"`), `tools` (`exiftool`, `ffmpeg`, `vips`, `heif_enc`) |
| `volumes_dir`, `volumes` | `"volumes"` and `["photos", "archive"]` |
| `counters` | `files`, `symlinks`, `indexable`, `junk`, `unreadable`, `total_bytes`, `skipped`, `by_kind`, `by_volume`, `dirs`, `empty_dirs`, `non_empty_dirs`, `duplicate_groups` (count per mode) |
| `scenarios` | object keyed by `group.name`: `group`, `description`, `files`, `dirs`, `skipped`, `skip_reason` |
| `duplicate_groups` | `mode1`, `mode2`, `mode3`, `mode4`, `multi`, `none`: arrays of groups (below) |
| `empty_dirs` | `[{volume, path, scenario}]`: folders without any indexable file below them |
| `non_empty_dirs` | the other indexable folders, same shape |
| `outside_files` | files outside the volumes that symlinks point to (`["outside/secret.jpg"]`; empty at large scale) |
| `skipped` | `[{scenario, reason}]` for scenarios this machine could not build |
| `files` | one object per file, symlink and skipped placeholder |

A file object:

| Field | Meaning |
|---|---|
| `volume`, `path` | volume name and path relative to the volume (byte-exact, no normalisation) |
| `type` | `file` or `symlink` |
| `size`, `sha1`, `mtime` | measured; `mtime` in epoch seconds; null for symlinks and skipped placeholders |
| `kind` | `image`, `raw`, `video`, `sidecar`, `other` or `junk` |
| `db_kind` | what the `files` table should hold: `Image` (also for `raw`), `Video`, `Sidecar`, `Other`, or null for junk and symlinks |
| `scenario` | the stable key; every file has one |
| `indexable` | false for junk, symlinks, skipped placeholders and files below an unreadable folder |
| `skip_reason` | `junk`, `symlink`, `unreadable_dir`, or the reason of a skipped scenario |
| `skipped` | true for a placeholder: the scenario could not be built on this machine and the file does not exist |
| `capture_ts_expected` | naive local `YYYY-MM-DDTHH:MM:SS`, or null when the indexer falls back to file times |
| `capture_src_expected` | `exif`, `xmp`, `video` or `fs` |
| `capture_offset_min_expected` | minutes east of UTC when an offset is embedded, else null |
| `tz_dependent` | true when `capture_ts_expected` only holds under `TZ=UTC` |
| `camera_expected` | normalised camera name or null |
| `rating_expected` | 0 to 5 (0 = unrated) |
| `flag_expected` | `selected`, `rejected` or `none` |
| `tags_expected` | sorted keyword components (hierarchies flattened) |
| `width`, `height`, `duration_ms` | pixel size and video length (about 1000 ms); null when not readable |
| `location_expected` | `{lat, lon}` or null |
| `sidecars` | for an owner: `[{path, link}]` with `link` = `exact` or `stem` |
| `owners` | for a sidecar: `[{path, link}]` |
| `groups` | ids of the duplicate groups the file belongs to |
| `unreadable` | null, `"file"` (mode 000 file) or `"dir"` (inside a mode 000 folder) |
| `symlink`, `link_target` | true and the target as written for symlinks |
| `note` | free text |

All `*_expected` fields, `width`, `height` and `duration_ms` are only filled for indexable images, RAW files and videos;
for every other file they are null (`rating_expected` and `flag_expected` too).

A duplicate group: `id`, `description`, `incidental` (true when it only exists as a side effect of another scenario),
`source` (`{volume, path}`, null for the controls group) and `members`. The source is the file the comparison starts
from, and a member is a counterpart; members never include the source:

```json
{
  "id": "multi_mode",
  "description": "One source, three kinds of match",
  "incidental": false,
  "source": { "volume": "photos", "path": "dupes/multi/IMG_0500.jpg" },
  "members": [
    { "volume": "archive", "path": "multi/IMG_0500.jpg", "role": "match", "modes": [1, 2, 4], "match": true, "match_plain_prefix": true },
    { "volume": "photos", "path": "dupes/multi/IMG_0500_edit.jpg", "role": "match", "modes": [4], "match": true, "match_plain_prefix": true },
    { "volume": "photos", "path": "dupes/multi/other/IMG_0500.jpg", "role": "match", "modes": [3, 4], "match": true, "match_plain_prefix": true }
  ]
}
```

`role` is `match` or `control`. `modes` lists the modes that must report the pair under the default rules (empty for a
control). `match` is true when the default rules report it at all. `match_plain_prefix` is true when mode 4 reports the
pair with the "strict prefix" toggle on, that is without the word boundary (the plan's A10): it holds for every mode 4
match, and for the two controls `IMG_04001` and `IMG_0400a`, which only match that way. Equal stems always match in mode
4, so every group whose names share a stem lists mode 4 as well.

A file entry, for `sidecar.exact` (one line in the file; hash and size depend on the toolset):

```json
{
  "volume": "photos", "path": "sidecars/IMG_0001.CR3", "type": "file", "size": 4187,
  "sha1": "3a1d80031b972e3bad89488383229bcc426e3a2f", "mtime": 1651570200, "kind": "raw", "db_kind": "Image",
  "scenario": "sidecar.exact", "indexable": true, "skip_reason": null, "skipped": false,
  "capture_ts_expected": "2022-05-03T09:30:00", "capture_src_expected": "xmp",
  "capture_offset_min_expected": null, "tz_dependent": false, "camera_expected": "Canon EOS R5",
  "rating_expected": 4, "flag_expected": "selected", "tags_expected": ["Italy", "Rome", "Travel"],
  "width": 160, "height": 120, "duration_ms": null, "location_expected": null,
  "sidecars": [{ "path": "sidecars/IMG_0001.CR3.xmp", "link": "exact" }], "owners": [], "groups": [],
  "unreadable": null, "symlink": false, "link_target": null, "note": null
}
```

## Flat TSV files

All start with a header line beginning with `#`. Null is `-`, so every field is non-empty and `read` with a tab
separator keeps the columns apart; paths may contain spaces, quotes and emoji but never tabs or newlines.

- `manifest.tsv`, 19 columns: `volume path type size sha1 mtime kind db_kind scenario indexable capture_ts capture_src
  capture_offset_min camera rating flag tags skip_reason link_target`. `indexable` is 0 or 1, `tags` is a sorted comma
  list. About 190 bytes per file, so it stays easy to stream at 200000 files. It has no rows for skipped placeholders
  (the JSON does), and the expected columns are `-` where the JSON has null.
- `manifest.dirs.tsv`: `volume path state scenario`, `state` is `empty` or `non_empty`, `scenario` is `-` for folders
  that belong to no scenario. Only indexable folders appear: junk folders and `perms/locked_dir` are absent.
- `manifest.dups.tsv`: `mode group volume path role match modes plain_prefix_only`. The `source` row comes first in each
  group (with `-` in the last three columns); `match` is 0 or 1, `modes` a comma list or `-`, and `plain_prefix_only` is
  1 for the controls that only match without the word boundary.

```sh
awk -F'\t' '$10 == 1 && $9 ~ /^bulk\./' fixtures/out-large/manifest.tsv | head      # indexable bulk files
jq -r '.files[] | select(.scenario == "dup.m1_pair") | "\(.volume):\(.path)"' fixtures/out/manifest.json
```

## What the expected values mean

They follow the plan (sections 5.3 and 5.4) and describe what a correct indexer stores.

- **Capture time**, in this priority: (1) EXIF DateTimeOriginal with SubSecTimeOriginal (the fraction is truncated) and
  OffsetTimeOriginal; (2) the date of an XMP sidecar; (3) CreateDate for images, the QuickTime `creationdate` or
  `creation_time` for videos (these are UTC, or carry an offset, and are converted); (4) the file birth time; (5) the
  modification time. Stored naive local, so the folder `2022/12` is the photographer's date. Birth time of generated
  files is the generation time, so `fs` rows have `capture_ts_expected` null: assert `capture_src` only. Rule (1)
  against rule (2) is not tested: `sidecar.exact` is the only sidecar that carries a date, and its owner has no
  embedded date.
- **Camera**: model and make are combined as `Make Model`, unless the model already starts with the make
  (case-insensitive), then the model alone.
- **Rating and flag**: `xmp:Rating` -1 means rating 0 and flag `rejected`. `xmpDM:pick` 1 is `selected`, -1 `rejected`.
  Sidecar values win field by field over embedded ones (`sidecar.override`).
- **Tags**: `dc:subject` and `lr:hierarchicalSubject`, with the hierarchy flattened into its components, sorted.
- **Sidecar links**, per folder and case-insensitive (also NFC/NFD-insensitive): `exact` when the sidecar name without
  its last extension equals the full name of the owner (`IMG_0001.CR3.xmp`), `stem` when the sidecar stem equals the
  owner stem (`IMG_0002.xmp`). Sidecar extensions: `xmp`, `json`, `aae`, `dop`, `pp3`, `xml` (not `txt`). Only `xmp`
  is parsed. A sidecar never links across folders, and `IMG_0004.xmp` does not link to `IMG_00041.jpg`.
- **Junk** (never indexed, folders never entered, case-insensitive): `@eaDir`, `#recycle`, `#snapshot`, `.opm-trash`,
  `.DS_Store`, `Thumbs.db`, `desktop.ini`, `._*`, `*.opm-part`. Symlinks are skipped too. `.hidden.jpg` is not junk.
- **Indexable** is false only for junk, symlinks and files below an unreadable folder. An unreadable file stays
  indexable (the scanner sees it; only its content cannot be read).
- **Empty folder**: a folder with no indexable file anywhere below it. Junk does not count, so a folder holding only
  `.DS_Store` or an `@eaDir` is empty, and so is a folder whose only children are empty.
- **Duplicates**: see the groups above. A pair is compared on the normalised file name (NFC, case-folded) and on the
  capture second; mode 4 is directional (the other stem starts with the source stem, and the next character is not an
  ASCII letter or digit, or the stems are equal).

## Using the fixtures in tests

- **Build once, cheaply.** Call `make fixtures` (or `fixtures/generate.sh`) from the global setup of a suite. It is
  idempotent, takes 0.3 s when current, and parallel callers queue on the lock. A test that needs the tree should skip
  cleanly when `fixtures/out/manifest.json` is missing and tell the user to run `make fixtures`.
- **Select by scenario key**, never by position or by a hard-coded path: filter `files` on `scenario` (and `volume`,
  `path`). Hashes, sizes and mtimes always come from the manifest.

  ```ts
  // Playwright / Node
  const manifest = JSON.parse(readFileSync('fixtures/out/manifest.json', 'utf8'));
  const filesOf = (key: string) => manifest.files.filter((f: any) => f.scenario === key);
  ```

  ```rust
  // Rust integration test
  let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/out/manifest.json");
  let manifest: serde_json::Value = serde_json::from_slice(&std::fs::read(path)?)?;
  let nikon = manifest["files"].as_array().unwrap().iter().find(|f| f["scenario"] == "exif.full_nikon");
  ```

- **Never modify the shared tree.** `fixtures/out` is read-only by convention. A test that moves, deletes, trashes or
  re-scans must work on its own tree: generate one per suite with `fixtures/generate.sh --out fixtures/out-NAME`
  (git-ignored by `fixtures/.gitignore`; about 9 s, then 0.3 s while untouched). A tree that a test changed is rebuilt
  automatically by the next `generate.sh` call, because folder names, file names, sizes and mtimes are part of the
  stamp.
- **Run with `TZ=UTC`** when asserting capture times of `tz_dependent` rows (the MP4). Everything else is independent of
  the time zone.
- **Skip the permission checks as root** (`id -u` is 0, typical in containers): the mode 000 entries are readable then.
- **Case and normalisation.** macOS volumes are case-insensitive and normalising, so the generator puts the NFC and NFD
  names, and `Casing.JPG` and `casing.jpg`, in different folders. Compare names through the normalised form, not byte
  by byte.
- **Skipped scenarios.** Check `skipped` before asserting on `media.gif`, `media.webp`, `media.heic`, the three
  `media.video_*`, `other.trailing_space_ext`, `names.unicode_nfc` and `names.unicode_nfd`: they are skipped (with a
  placeholder row) where the encoder or the filesystem cannot provide them.
- **Load tests**: `make fixtures-large` once, point `OPM_VOLUMES_DIR` at `fixtures/out-large/volumes`, and read
  `manifest.tsv`.

## verify.sh

```
fixtures/verify.sh [--out DIR] [--full | --sample N] [--no-regen]
```

Checks a generated tree against its manifest, using only the shell tools:

1. size and mtime of every file against `manifest.tsv`, with no missing and no unexpected file; the symlinks and their
   targets, the folder lists, and the locked entries (skipped as root);
2. sha1 of every file up to 25000 files; above that a deterministic sample of the bulk files (`--sample N`, 5000 by
   default) plus every 40th mode 1 group; `--full` hashes everything;
3. an exiftool re-read of capture time, offset and camera for the images and RAW files whose expected source is `exif`
   (the same sample), and a check that the three broken JPEGs stay unreadable for exiftool;
4. a determinism check: regenerate into `OUT-verify` and compare the set of (volume, path, size, sha1, mtime) and the
   bytes of `manifest.json` (`--no-regen` skips it; it costs as much as a generation).

Every step runs and reports its first problems; the exit status is non-zero when any step found one. A tree that
`mutations.sh` changed is refused. Small scale takes about 10 s; 3000 files take 6 s and 20000 files 30 s (200000 files:
19 s with `--no-regen`).

## mutations.sh

```
fixtures/mutations.sh [--out DIR] [--list]
```

Applies seven deterministic changes to a small-scale tree, so incremental-scan and watcher tests have something to
find. `--list` prints them without applying. It works on the files of the `stale/` folder of `volumes/photos`, writes
`OUT/mutations.tsv` (`op volume path new_path size sha1 mtime note`) with the new state, and leaves `manifest.json`
describing the baseline.

| Op | File | Change |
|---|---|---|
| `rename` | `stale/rename_me.jpg` | renamed to `stale/renamed.jpg`, same bytes |
| `change_bytes` | `stale/bytes_changed.jpg` | one byte of the JPEG header flipped (a quantisation table entry: the file still decodes, EXIF is unchanged); size and mtime stay the same |
| `touch` | `stale/mtime_only.jpg` | mtime becomes 2024-06-15T12:00:00Z, bytes unchanged |
| `delete_dir` | `stale/doomed_dir` | folder with three photos removed |
| `add_sidecar` | `stale/has_new_sidecar.xmp` | new sidecar (rating 4) next to `has_new_sidecar.jpg` |
| `edit_sidecar` | `stale/touched.xmp` | rating 2 becomes 3 (same size), new mtime |
| `add_file` | `stale/added_new.jpg` | a new photo, one byte different from `mtime_only.jpg` |

Use it on a dedicated tree, never on the shared one:

```sh
fixtures/generate.sh --out fixtures/out-stale
# run the first scan, then:
fixtures/mutations.sh --out fixtures/out-stale
# run the second scan and compare with fixtures/out-stale/mutations.tsv
fixtures/generate.sh --out fixtures/out-stale     # rebuilds the baseline (the stamp no longer matches)
```

A second `mutations.sh` run is refused, and so is a large-scale tree. `verify.sh` refuses a mutated tree.

## Assumptions and limits

Decisions taken where the plan is silent. Tests should treat them as the contract of this generator.

- **Time zone.** Expected capture times assume `TZ=UTC`; only `media.video_mp4` is `tz_dependent` (its `creation_time`
  is UTC and the indexer stores local time). The MOV carries an explicit offset and does not depend on the zone.
- **Rating -1** is stored as rating 0 plus the flag `rejected`.
- **CreateDate only** counts as source `exif`. A QuickTime `0000:00:00 00:00:00` date (`media.video_nometa`) is absent.
- **File-time fallback** has a null `capture_ts_expected` because the birth time of a generated file is not stable.
- **Broken files** (`exif.corrupt_header`, `exif.truncated`, `exif.zero_byte`): the assertion is that the file is
  indexed without a crash and falls back to file times. Other readers may salvage tags from a damaged header.
- **Unreadable file** (`perms/unreadable.jpg`): stays indexable with source `fs`. Its `width` and `height` are what the
  file really has (160x120), but a non-root indexer cannot open it, so do not assert them while `unreadable` is set.
  Running as root makes the file readable (the manifest does not change).
- **Leading-dot files** (`.hidden.jpg`) are not junk; the junk list is the one under "What the expected values mean".
- **RAW stand-ins** are JPEG or TIFF files with a RAW extension (real RAW cannot be synthesised with these tools).
- **Optional formats.** `media.heic` needs `heif-enc`; `media.gif`, `media.webp` and the three `media.video_*` are skipped
  when the encoder is missing or fails (vips without WebP or GIF support, ffmpeg without libx264); `other.trailing_space_ext`,
  `names.unicode_nfc` and `names.unicode_nfd` are skipped when the filesystem cannot keep such names. Each one gets an
  entry in `skipped` and a placeholder row in `manifest.json` (no row in the TSV files), never an error.
- **Large scale is bulk only.** None of the small-scale scenarios exist there (no symlinks, junk or empty folders).
- **Tool versions** change the encoded bytes (not the names or metadata); they are part of the signature, so a
  different toolset rebuilds and the hashes differ from another machine's.
- **Not covered**: real RAW decoding, EXIF orientation, bursts, Live Photos, and filesystem birth times.

## Extending the catalogue

Add a block to a function of `fixtures/lib/small.sh` (one function per group, all called from `small_catalogue`):

```bash
scn exif.my_case exif "What this file is and what the indexer must do with it"
img photos 2021/05/IMG_9999.jpg dt=2021-05-05T05:05:05 make=Canon model="Canon EOS R5" mtime=2021-05-05T12:00:00
```

The expected values in the manifest are derived from the options (and measured from the finished tree), so they cannot
drift from the file. The helpers live in `fixtures/lib/media.sh`:

| Helper | Purpose |
|---|---|
| `scn KEY GROUP "description"` | starts a scenario; the files created after it carry its key |
| `skip_scn KEY REASON [VOLUME PATH]` | declares a scenario that cannot be built here (a placeholder row when a path is given) |
| `img VOLUME PATH [options]` | renders, tags and places an image and sets `FX_KEY` |
| `put KEY VOLUME PATH [options]` | another placement of an image, with identical bytes (`src=$FX_KEY` after `img`, then `put "$src" ...`) |
| `put_empty VOLUME PATH`, `put_text VOLUME PATH TEXT`, `put_bytes VOLUME PATH PRINTF_FORMAT`, `put_junk VOLUME PATH PRINTF_FORMAT`, `put_xmp VOLUME PATH [xmp options]` | empty, text, binary, junk and XMP files |
| `video VOLUME PATH mp4\|mov\|nometa [options]` | a one-second clip |
| `put_dated VOLUME PATH CAMERA VARIANT ISO_DATE` | a 16x12 JPEG from a pre-tagged template, with no process per file (used for the grid) |
| `mk_symlink VOLUME PATH TARGET`, `empty_dir VOLUME PATH [KEY]`, `dir_scn VOLUME PATH [KEY]` | symlinks, empty folders, and the scenario shown for a folder in `manifest.dirs.tsv` |
| `sc_link VOLUME OWNER SIDECAR exact\|stem` | declares a sidecar link the indexer must create |
| `dupgroup ID MODE "description" [incidental]`, `dupsrc ID VOLUME PATH`, `dupmem ID VOLUME PATH MODES [plain]` | declares a duplicate group; `MODES` is a comma list of modes (`-` for a control) and `plain` marks a control that only matches without the word boundary |
| `can_name NAME` | succeeds when the filesystem keeps this exact file name |

Options are `key=value` pairs, and an unknown key stops the generator:

- Image: `fmt` (`jpg`, `png`, `gif`, `webp`, `tif`, `heic`; default from the extension), `size=WxH` (default `160x120`),
  `seed`, `noise=1` (about 1.5 MB at 1920x1280), `tag=0` (no metadata), `opt=1` (a missing encoder skips the scenario
  instead of failing).
- Metadata to write: `dt` and `cd` (DateTimeOriginal and CreateDate, `YYYY-MM-DDTHH:MM:SS`), `sub`, `offset`
  (`+02:00`), `make`, `model`, `rating` (-1 to 5), `pick` (1 or -1), `kw` (comma list), `hier` (`A|B|C`),
  `gps=LAT,LON`.
- Overrides of the derived expectation: `x_cap`, `x_src`, `x_off`, `x_tzd`, `x_camera`, `x_rating`, `x_flag`, `x_tags`;
  `xmp=1` marks an owner whose sidecar was written by the preceding `put_xmp` (its values are folded in).
  `put_xmp` takes `rating`, `pick`, `make`, `model`, `date`, `kw`, `hier` and `style=attr|elem`.
- Other: `kind=junk`, `note`, `mtime` (`YYYY-MM-DDTHH:MM:SS`, UTC), `scn` (another scenario key), `unread=file|dir`,
  `corrupt=header|trunc|zero`.

Rules:

- The manifest builder recomputes the sidecar links and the duplicate matches from names, hashes and capture times, and
  fails the generation with the pair named on any difference from what the catalogue declared, including an
  undeclared match between two files (the pairwise check that `--no-exhaustive` turns off).
- Keep it deterministic: no dates, random numbers or process ids in file content; give an explicit `mtime` when it
  matters (without one, a file with a capture date gets that date and any other file 2020-01-01T12:00:00Z); use unique
  stems unless a duplicate is intended.
- Run `fixtures/verify.sh` (and `make fixtures` twice) before landing. Never rename an existing key: new scenarios add
  keys.
