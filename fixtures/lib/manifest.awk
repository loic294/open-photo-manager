# manifest.awk: joins the registry with the measured tree, cross-checks the catalogue and writes the manifest sections.
# Plain POSIX awk (macOS awk, mawk and gawk all run it). Input files are told apart by FILENAME.
#
#   -v F_STAT F_SHA F_REG F_LINKS F_GRP F_DUPS F_DIRSCN F_SCN F_SKIP F_DIRS   input files (see generate.sh)
#   -v O=PREFIX   output prefix: .files.json .tsv .dirs.json .dirs.tsv .grp.{1,2,3,4,multi,none} .dups.tsv
#                 .scn.json .counters.json .skipped.json .empty.json .nonempty.json
#   -v EXH=0|1    exhaustive pairwise duplicate check (default 1)
#
# Every inconsistency is reported on stderr as "ASSERT: ..." and makes the exit status 1: a generated fixture tree
# must agree with the links, duplicate groups and scenarios the catalogue declared.

function bad(msg) {
  nbad++
  if (nbad <= 60) printf "ASSERT: %s\n", msg > "/dev/stderr"
}

function js(s,   i, n, c, out) {
  if (s !~ /["\\]/) return "\"" s "\""
  out = ""
  n = length(s)
  for (i = 1; i <= n; i++) {
    c = substr(s, i, 1)
    if (c == "\"") out = out "\\\""
    else if (c == "\\") out = out "\\\\"
    else out = out c
  }
  return "\"" out "\""
}

function jnum(s) { return (s == "-" || s == "") ? "null" : s }
function jstr(s) { return (s == "-" || s == "") ? "null" : js(s) }
function jbool(s) { return (s == "1") ? "true" : "false" }

function jtags(s,   n, a, i, out) {
  if (s == "-" || s == "") return "[]"
  n = split(s, a, ",")
  out = "["
  for (i = 1; i <= n; i++) out = out (i > 1 ? "," : "") js(a[i])
  return out "]"
}

function jloc(s,   p) {
  if (s == "-" || s == "") return "null"
  p = index(s, ",")
  return "{\"lat\":" substr(s, 1, p - 1) ",\"lon\":" substr(s, p + 1) "}"
}

function cell(s) { return (s == "") ? "-" : s }

# NFC/lower-case comparison key for names: ASCII case folding plus the decomposed e-acute spelling.
function nrm(s) {
  s = tolower(s)
  gsub("e\314\201", "\303\251", s)
  return s
}

# psplit PATH: P_DIR and P_NAME ("" directory for a top-level name)
function psplit(p) {
  if (match(p, /\/[^\/]*$/)) {
    P_DIR = substr(p, 1, RSTART - 1)
    P_NAME = substr(p, RSTART + 1)
  } else {
    P_DIR = ""
    P_NAME = p
  }
}

# split_name NAME: S_STEM and S_EXT; a leading dot belongs to the stem (opm_core::names::split_name).
function split_name(name,   rest) {
  rest = substr(name, 2)
  if (match(rest, /\.[^.]*$/)) {
    S_STEM = substr(name, 1, RSTART)
    S_EXT = substr(rest, RSTART + 1)
  } else {
    S_STEM = name
    S_EXT = ""
  }
}

function junk_name(n,   l) {
  l = tolower(n)
  if (l == "@eadir" || l == "#recycle" || l == "#snapshot" || l == ".opm-trash") return 1
  if (l == ".ds_store" || l == "thumbs.db" || l == "desktop.ini") return 1
  if (substr(l, 1, 2) == "._") return 1
  if (length(l) >= 9 && substr(l, length(l) - 8) == ".opm-part") return 1
  return 0
}

function isjunk(path,   n, parts, i) {
  n = split(path, parts, "/")
  for (i = 1; i <= n; i++) if (junk_name(parts[i])) return 1
  return 0
}

function under_unreadable(vol, rel,   n, parts, i, p) {
  n = split(rel, parts, "/")
  p = ""
  for (i = 1; i <= n; i++) {
    p = (i == 1) ? parts[1] : p "/" parts[i]
    if ((vol SUBSEP p) in UNRDIR) return 1
  }
  return 0
}

function mark_nonempty(vol, dir,   n, parts, i, p) {
  if (dir == "" || ((vol SUBSEP dir) in NE)) return
  n = split(dir, parts, "/")
  p = ""
  for (i = 1; i <= n; i++) {
    p = (i == 1) ? parts[1] : p "/" parts[i]
    NE[vol SUBSEP p] = 1
  }
}

function dir_scenario(vol, rel,   p) {
  p = rel
  while (p != "") {
    if ((vol SUBSEP p) in DIRSCN) return DIRSCN[vol SUBSEP p]
    if (!match(p, /\/[^\/]*$/)) break
    p = substr(p, 1, RSTART - 1)
  }
  return ""
}

function rowkey(i,   ff) {
  split(LINE[i], ff, "\t")
  return ff[1] "/" ff[2]
}

function rowpath(i,   ff) {
  split(LINE[i], ff, "\t")
  return ff[2]
}

function has_mode(list, m) { return index("," list ",", "," m ",") > 0 }

function addset(first, multi, key, id) {
  # first[key] = first id, multi[key] = every id once a second one shows up (sets of one stay cheap)
  if (key in first) {
    if (key in multi) multi[key] = multi[key] " " id
    else multi[key] = first[key] " " id
  } else {
    first[key] = id
  }
}

function prepare(   i, n, f, vol, path, type, kind, idx, k, nm, st, h) {
  for (i = 1; i <= NREG; i++) {
    n = split(LINE[i], f, "\t")
    vol = f[1]; path = f[2]; type = f[3]; kind = f[4]; idx = f[15]
    k = vol "/" path
    if (n != 23) bad("registry row " k " has " n " columns")
    if (!(vol in VOLOK)) bad("unknown volume in " k)
    if (!(f[6] in SIDX)) bad("undeclared scenario " f[6] " used by " k)
    if (type == "skipped") {
      NSKIPROWS++
      continue
    }
    SC_FILES[f[6]]++
    if ((kind == "junk") != isjunk(path)) bad("junk classification differs from the name rules for " k)
    if (type == "symlink") {
      NSYM++
      if (k in MT) bad("symlink " k " was listed as a regular file")
      continue
    }
    if (type != "file") bad("unknown row type " type " for " k)
    if (!(k in MT)) bad("registered file missing on disk: " k)
    if (!(k in SHA)) bad("no sha1 for " k)
    NFILES++
    BYKIND[kind]++
    BYVOL[vol]++
    TOTB += SZ[k]
    if (idx == "1") NIDX++
    if (kind == "junk") NJUNK++
    if (f[17] != "-") NUNR++
    if (f[17] == "dir") {
      psplit(path)
      UNRDIR[vol SUBSEP P_DIR] = 1
    }
    psplit(path)
    if (idx == "1") mark_nonempty(vol, P_DIR)
    if (idx != "1") continue
    if (kind == "sidecar") {
      SCID[++NSC] = i
    } else if (kind == "image" || kind == "raw" || kind == "video") {
      split_name(P_NAME)
      nm = nrm(P_NAME)
      st = nrm(S_STEM)
      OWNF[vol SUBSEP P_DIR SUBSEP nm] = OWNF[vol SUBSEP P_DIR SUBSEP nm] " " i
      OWNS[vol SUBSEP P_DIR SUBSEP st] = OWNS[vol SUBSEP P_DIR SUBSEP st] " " i
      MED[++NMED] = i
      if (EXH) {
        addset(HFIRST, HMULTI, SHA[k], i)
        addset(NFIRST, NMULTI, nm, i)
        addset(SFIRST, SMULTI, st, i)
      }
    }
  }
  for (k in MT) if (!(k in IDX)) bad("file on disk without a registry row: " k)
  for (k in SHA) if (!(k in IDX)) bad("sha1 listed for a path without a registry row: " k)
}

function check_scenarios(   j, key) {
  for (j = 1; j <= NSK; j++) {
    key = SK_KEY[j]
    if (!(key in SIDX)) bad("skipped scenario " key " is not declared")
  }
  for (j = 1; j <= NS; j++) {
    key = S_KEY[j]
    if (!(key in SK) && SC_FILES[key] == 0 && SC_DIRS[key] == 0) bad("scenario " key " has neither files nor folders")
    if ((key in SK) && SC_FILES[key] != 0) bad("skipped scenario " key " still has files")
  }
}

function add_link(o, s, lk) {
  LK[o SUBSEP s] = lk
  SCL[o] = SCL[o] " " s ":" lk
  OWL[s] = OWL[s] " " o ":" lk
}

# Sidecar links computed from the file names must equal the links the catalogue declared, in both directions.
function check_links(   s, i, f, vol, skey, n, parts, j, o, d, io, is, key) {
  for (s = 1; s <= NSC; s++) {
    i = SCID[s]
    split(LINE[i], f, "\t")
    vol = f[1]
    psplit(f[2])
    split_name(P_NAME)
    skey = vol SUBSEP P_DIR SUBSEP nrm(S_STEM)
    if (skey in OWNF) {
      n = split(OWNF[skey], parts, " ")
      for (j = 1; j <= n; j++) add_link(parts[j] + 0, i, "exact")
    }
    if (skey in OWNS) {
      n = split(OWNS[skey], parts, " ")
      for (j = 1; j <= n; j++) {
        o = parts[j] + 0
        if (!((o SUBSEP i) in LK)) add_link(o, i, "stem")
      }
    }
  }
  for (d = 1; d <= NDL; d++) {
    io = DL_V[d] "/" DL_O[d]
    is = DL_V[d] "/" DL_S[d]
    if (!(io in IDX) || !(is in IDX)) {
      bad("declared link references an unknown file: " io " <- " is)
      continue
    }
    key = IDX[io] SUBSEP IDX[is]
    DLSEEN[key] = 1
    if (!(key in LK)) bad("declared sidecar link is not produced by the name rules: " io " <- " is)
    else if (LK[key] != DL_K[d]) bad("sidecar link kind differs (" DL_K[d] " declared, " LK[key] " computed): " io " <- " is)
  }
  for (key in LK) {
    if (!(key in DLSEEN)) {
      split(key, parts, SUBSEP)
      bad("undeclared sidecar link: " rowkey(parts[1]) " <- " rowkey(parts[2]))
    }
  }
}

function load_props(i, w,   f) {
  split(LINE[i], f, "\t")
  psplit(f[2])
  split_name(P_NAME)
  PR_SHA[w] = SHA[f[1] "/" f[2]]
  PR_NM[w] = nrm(P_NAME)
  PR_ST[w] = nrm(S_STEM)
  PR_CAP[w] = f[7]
}

# compute_modes SOURCE_ROW CANDIDATE_ROW: comma list of the default-rule match modes, "-" for none; CM_PLAIN is 1
# when the only relation is a prefix that the word-boundary rule rejects (it matches under strict_prefix).
function compute_modes(a, b,   m, la, sa, sb) {
  load_props(a, 1)
  load_props(b, 2)
  m = ""
  CM_PLAIN = 0
  if (PR_SHA[1] != "" && PR_SHA[1] == PR_SHA[2]) m = "1"
  if (PR_NM[1] == PR_NM[2]) m = m (m == "" ? "" : ",") (PR_CAP[1] == PR_CAP[2] ? "2" : "3")
  sa = PR_ST[1]
  sb = PR_ST[2]
  la = length(sa)
  if (la > 0 && substr(sb, 1, la) == sa) {
    if (length(sb) == la) m = m (m == "" ? "" : ",") "4"
    else if (substr(sb, la + 1, 1) ~ /[A-Za-z0-9]/) CM_PLAIN = 1
    else m = m (m == "" ? "" : ",") "4"
  }
  return (m == "" ? "-" : m)
}

function check_groups(   g, id, mode, n, parts, j, d, k, row, nsrc, src, srcd, cm, decl, any) {
  for (g = 1; g <= NG; g++) {
    id = G_ID[g]
    mode = G_MODE[g]
    if (!(mode in OKMODE)) bad("group " id " has unknown mode " mode)
    n = split(GM[g], parts, " ")
    if (n == 0) bad("group " id " has no files")
    nsrc = 0
    src = 0
    for (j = 1; j <= n; j++) {
      d = parts[j] + 0
      k = D_V[d] "/" D_P[d]
      if (!(k in IDX)) {
        bad("group " id " references an unknown file " k)
        continue
      }
      row = IDX[k]
      D_ROW[d] = row
      GL[row] = GL[row] " " id
      if (mode == "none") INNONE[row] = 1
      if (D_ROLE[d] == "source") {
        nsrc++
        src = row
      }
    }
    if (mode == "none") {
      if (nsrc != 0) bad("group " id " (none) must not have a source")
      for (j = 1; j <= n; j++) {
        d = parts[j] + 0
        if (D_MODES[d] != "-" || D_PLAIN[d] != "0") bad("group " id " (none) lists a matching member " D_V[d] "/" D_P[d])
      }
      continue
    }
    if (nsrc != 1) {
      bad("group " id " needs exactly one source, has " nsrc)
      continue
    }
    any = 0
    for (j = 1; j <= n; j++) {
      d = parts[j] + 0
      if (D_ROLE[d] == "source" || !(d in D_ROW)) continue
      cm = compute_modes(src, D_ROW[d])
      decl = D_MODES[d]
      if (cm != decl) bad("group " id ": " D_V[d] "/" D_P[d] " matches in modes " cm " but the catalogue declared " decl)
      if (CM_PLAIN != D_PLAIN[d] + 0) bad("group " id ": plain-prefix flag differs for " D_V[d] "/" D_P[d])
      if (decl != "-") {
        any = 1
        if (mode != "multi" && !has_mode(decl, mode)) bad("group " id ": member " D_V[d] "/" D_P[d] " does not match in the group mode " mode)
      }
    }
    if (!any) bad("group " id " has no matching member")
  }
}

function common_group(a, b,   n, parts, j) {
  if (!(a in GL) || !(b in GL)) return 0
  n = split(GL[a], parts, " ")
  for (j = 1; j <= n; j++) if (index(GL[b] " ", " " parts[j] " ") > 0) return 1
  return 0
}

function check_pair(a, b, why) {
  if ((a in INNONE) || (b in INNONE)) {
    bad("negative control matches something (" why "): " rowkey(a) " and " rowkey(b))
    return
  }
  if (!common_group(a, b)) bad("undeclared " why " match: " rowkey(a) " and " rowkey(b))
}

function check_set(list, why,   n, parts, x, y) {
  n = split(list, parts, " ")
  for (x = 1; x < n; x++) for (y = x + 1; y <= n; y++) check_pair(parts[x] + 0, parts[y] + 0, why)
}

# Every pair of media files that the default or strict rules would report must belong to a declared group.
function check_exhaustive(   h, m, b, f, s, l, k, p, ids, nn, parts, jj, a) {
  for (h in HMULTI) check_set(HMULTI[h], "hash")
  for (h in NMULTI) check_set(NMULTI[h], "same-name")
  for (m = 1; m <= NMED; m++) {
    b = MED[m]
    split(LINE[b], f, "\t")
    psplit(f[2])
    split_name(P_NAME)
    s = nrm(S_STEM)
    l = length(s)
    for (k = 1; k <= l; k++) {
      p = substr(s, 1, k)
      if (!(p in SFIRST)) continue
      ids = (p in SMULTI) ? SMULTI[p] : SFIRST[p]
      nn = split(ids, parts, " ")
      for (jj = 1; jj <= nn; jj++) {
        a = parts[jj] + 0
        if (a == b) continue
        if (k == l && a > b) continue
        check_pair(a, b, "name-prefix")
      }
    }
  }
}

function emit_dirs(   i, line, p, vol, rel, scn, st) {
  for (i = 1; i <= NDIRL; i++) {
    line = DIRL[i]
    p = index(line, "/")
    if (p == 0) continue
    vol = substr(line, 1, p - 1)
    rel = substr(line, p + 1)
    if (isjunk(rel) || under_unreadable(vol, rel)) continue
    NDIRS++
    scn = dir_scenario(vol, rel)
    if ((vol SUBSEP rel) in NE) {
      st = "non_empty"
      printf "%s{\"volume\":%s,\"path\":%s,\"scenario\":%s}", (NNE++ ? ",\n" : "\n"), js(vol), js(rel), jstr(scn) >> FNE
    } else {
      st = "empty"
      printf "%s{\"volume\":%s,\"path\":%s,\"scenario\":%s}", (NEM++ ? ",\n" : "\n"), js(vol), js(rel), jstr(scn) >> FEM
    }
    printf "%s\t%s\t%s\t%s\n", vol, rel, st, cell(scn) >> FDT
  }
}

function emit_groups(   g, id, mode, n, parts, j, d, src, members, nm, modes, plain, matched, role, pm) {
  for (g = 1; g <= NG; g++) {
    id = G_ID[g]
    mode = G_MODE[g]
    n = split(GM[g], parts, " ")
    src = "null"
    members = ""
    nm = 0
    for (j = 1; j <= n; j++) {
      d = parts[j] + 0
      if (D_ROLE[d] == "source") {
        src = "{\"volume\":" js(D_V[d]) ",\"path\":" js(D_P[d]) "}"
        printf "%s\t%s\t%s\t%s\tsource\t-\t-\t-\n", mode, id, D_V[d], D_P[d] >> FDU
        continue
      }
      modes = D_MODES[d]
      plain = D_PLAIN[d]
      matched = (modes != "-" && mode != "none")
      role = matched ? "match" : "control"
      pm = (plain == "1" || has_mode(modes, 4)) ? "true" : "false"
      members = members (nm++ ? "," : "") "{\"volume\":" js(D_V[d]) ",\"path\":" js(D_P[d]) ",\"role\":\"" role "\",\"modes\":[" (modes == "-" ? "" : modes) "],\"match\":" (matched ? "true" : "false") ",\"match_plain_prefix\":" pm "}"
      printf "%s\t%s\t%s\t%s\t%s\t%d\t%s\t%s\n", mode, id, D_V[d], D_P[d], role, matched, modes, plain >> FDU
    }
    printf "%s{\"id\":%s,\"description\":%s,\"incidental\":%s,\"source\":%s,\"members\":[%s]}", (GB[mode]++ ? ",\n" : "\n"), js(id), js(G_DESC[g]), (G_FLAG[g] == "incidental" ? "true" : "false"), src, members >> (O ".grp." mode)
  }
}

function linklist(s,   n, a, j, t, out) {
  if (s == "") return "[]"
  n = split(s, a, " ")
  out = "["
  for (j = 1; j <= n; j++) {
    t = index(a[j], ":")
    out = out (j > 1 ? "," : "") "{\"path\":" js(rowpath(substr(a[j], 1, t - 1) + 0)) ",\"link\":" js(substr(a[j], t + 1)) "}"
  }
  return out "]"
}

function idlist(s,   n, a, j, out) {
  if (s == "") return "[]"
  n = split(s, a, " ")
  out = "["
  for (j = 1; j <= n; j++) out = out (j > 1 ? "," : "") js(a[j])
  return out "]"
}

function emit_row(i,   f, vol, path, type, kind, dbk, scn, cap, src, off, tzd, cam, rating, flag, tags, idx, note, unr, lnk, w, h, dur, loc, skip, k, size, sha, mt, media, placeholder, o) {
  split(LINE[i], f, "\t")
  vol = f[1]; path = f[2]; type = f[3]; kind = f[4]; dbk = f[5]; scn = f[6]
  cap = f[7]; src = f[8]; off = f[9]; tzd = f[10]; cam = f[11]; rating = f[12]; flag = f[13]; tags = f[14]
  idx = f[15]; note = f[16]; unr = f[17]; lnk = f[18]; w = f[19]; h = f[20]; dur = f[21]; loc = f[22]; skip = f[23]
  k = vol "/" path
  placeholder = (type == "skipped")
  size = "-"; sha = "-"; mt = "-"
  if (type == "file") {
    size = SZ[k]; sha = SHA[k]; mt = MT[k]
  }
  media = (type == "file" && idx == "1" && (kind == "image" || kind == "raw" || kind == "video"))
  if (!media) {
    cap = "-"; src = "-"; off = "-"; tzd = "0"; cam = "-"; rating = "-"; flag = "-"; tags = "-"; w = "-"; h = "-"; dur = "-"; loc = "-"
  }
  o = "{\"volume\":" js(vol) ",\"path\":" js(path) ",\"type\":" js(placeholder ? "file" : type) ",\"size\":" jnum(size) ",\"sha1\":" jstr(sha) ",\"mtime\":" jnum(mt) \
    ",\"kind\":" js(kind) ",\"db_kind\":" jstr(dbk) ",\"scenario\":" js(scn) ",\"indexable\":" jbool(idx) ",\"skip_reason\":" jstr(skip) ",\"skipped\":" (placeholder ? "true" : "false") \
    ",\"capture_ts_expected\":" jstr(cap) ",\"capture_src_expected\":" jstr(src) ",\"capture_offset_min_expected\":" jnum(off) ",\"tz_dependent\":" jbool(tzd) \
    ",\"camera_expected\":" jstr(cam) ",\"rating_expected\":" jnum(rating) ",\"flag_expected\":" jstr(flag) ",\"tags_expected\":" jtags(tags) \
    ",\"width\":" jnum(w) ",\"height\":" jnum(h) ",\"duration_ms\":" jnum(dur) ",\"location_expected\":" jloc(loc) \
    ",\"sidecars\":" linklist(SCL[i]) ",\"owners\":" linklist(OWL[i]) ",\"groups\":" idlist(GL[i]) \
    ",\"unreadable\":" jstr(unr) ",\"symlink\":" (type == "symlink" ? "true" : "false") ",\"link_target\":" jstr(lnk) ",\"note\":" jstr(note) "}"
  printf "%s%s", (NOUT++ ? ",\n" : "\n"), o >> FJ
  if (!placeholder) {
    printf "%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\n", vol, path, type, cell(size), cell(sha), cell(mt), kind, cell(dbk), scn, idx, \
      cell(cap), cell(src), cell(off), cell(cam), cell(rating), cell(flag), cell(tags), cell(skip), cell(lnk) >> FT
  }
}

function emit_scenarios(   j, key) {
  for (j = 1; j <= NS; j++) {
    key = S_KEY[j]
    printf "%s%s:{\"group\":%s,\"description\":%s,\"files\":%d,\"dirs\":%d,\"skipped\":%s,\"skip_reason\":%s}", (j > 1 ? ",\n" : ""), js(key), js(S_GRP[j]), js(S_DESC[j]), SC_FILES[key], SC_DIRS[key], ((key in SK) ? "true" : "false"), ((key in SK) ? js(SK[key]) : "null") >> FS_
  }
  for (j = 1; j <= NSK; j++) printf "%s{\"scenario\":%s,\"reason\":%s}", (j > 1 ? ",\n" : ""), js(SK_KEY[j]), js(SK[SK_KEY[j]]) >> FSK
}

function emit_counters(   j, k, kinds, nk, vols, nv) {
  nk = split("image raw video sidecar other junk", kinds, " ")
  nv = split("photos archive", vols, " ")
  printf "{\"files\":%d,\"symlinks\":%d,\"indexable\":%d,\"junk\":%d,\"unreadable\":%d,\"total_bytes\":%.0f,\"skipped\":%d,", NFILES, NSYM, NIDX, NJUNK, NUNR, TOTB, NSK > FC
  printf "\"by_kind\":{" >> FC
  for (j = 1; j <= nk; j++) printf "%s\"%s\":%d", (j > 1 ? "," : ""), kinds[j], BYKIND[kinds[j]] >> FC
  printf "},\"by_volume\":{" >> FC
  for (j = 1; j <= nv; j++) printf "%s\"%s\":%d", (j > 1 ? "," : ""), vols[j], BYVOL[vols[j]] >> FC
  printf "},\"dirs\":%d,\"empty_dirs\":%d,\"non_empty_dirs\":%d,", NDIRS, NEM, NNE >> FC
  printf "\"duplicate_groups\":{\"mode1\":%d,\"mode2\":%d,\"mode3\":%d,\"mode4\":%d,\"multi\":%d,\"none\":%d}}", GB["1"], GB["2"], GB["3"], GB["4"], GB["multi"], GB["none"] >> FC
}

BEGIN {
  FS = "\t"
  if (EXH == "") EXH = 1
  VOLOK["photos"] = 1
  VOLOK["archive"] = 1
  split("1 2 3 4 multi none", MODELIST, " ")
  for (i = 1; i <= 6; i++) {
    OKMODE[MODELIST[i]] = 1
    GB[MODELIST[i]] = 0
    printf "" > (O ".grp." MODELIST[i])
  }
  FJ = O ".files.json"; FT = O ".tsv"; FDT = O ".dirs.tsv"; FDU = O ".dups.tsv"
  FNE = O ".nonempty.json"; FEM = O ".empty.json"; FS_ = O ".scn.json"; FSK = O ".skipped.json"; FC = O ".counters.json"
  printf "" > FJ
  printf "" > FT
  printf "" > FDT
  printf "" > FDU
  printf "" > FNE
  printf "" > FEM
  printf "" > FS_
  printf "" > FSK
}

{
  if ($0 == "") next
  if (FILENAME == F_STAT) {
    MT[$3] = $1
    SZ[$3] = $2
  } else if (FILENAME == F_SHA) {
    SHA[substr($0, 43)] = substr($0, 1, 40)
  } else if (FILENAME == F_REG) {
    NREG++
    LINE[NREG] = $0
    if (($1 "/" $2) in IDX) bad("duplicate registry entry " $1 "/" $2)
    IDX[$1 "/" $2] = NREG
  } else if (FILENAME == F_LINKS) {
    NDL++
    DL_V[NDL] = $1; DL_O[NDL] = $2; DL_S[NDL] = $3; DL_K[NDL] = $4
  } else if (FILENAME == F_GRP) {
    NG++
    G_ID[NG] = $1; G_MODE[NG] = $2; G_DESC[NG] = $3; G_FLAG[NG] = $4
    if ($1 in GIDX) bad("duplicate duplicate-group id " $1)
    GIDX[$1] = NG
  } else if (FILENAME == F_DUPS) {
    NDU++
    D_G[NDU] = $1; D_V[NDU] = $2; D_P[NDU] = $3; D_ROLE[NDU] = $4; D_MODES[NDU] = $5; D_PLAIN[NDU] = $6
    if (!($1 in GIDX)) bad("duplicate-group member for an undeclared group " $1)
    else GM[GIDX[$1]] = GM[GIDX[$1]] " " NDU
  } else if (FILENAME == F_DIRSCN) {
    DIRSCN[$1 SUBSEP $2] = $3
    SC_DIRS[$3]++
    DSCN_LIST[++NDSC] = $3
  } else if (FILENAME == F_SCN) {
    NS++
    S_KEY[NS] = $1; S_GRP[NS] = $2; S_DESC[NS] = $3
    if ($1 in SIDX) bad("scenario " $1 " declared twice")
    SIDX[$1] = NS
    SC_FILES[$1] = 0
    if (!($1 in SC_DIRS)) SC_DIRS[$1] = 0
  } else if (FILENAME == F_SKIP) {
    NSK++
    SK_KEY[NSK] = $1
    SK[$1] = $2
  } else if (FILENAME == F_DIRS) {
    DIRL[++NDIRL] = $0
  }
}

END {
  prepare()
  for (i = 1; i <= NDSC; i++) if (!(DSCN_LIST[i] in SIDX)) bad("folder mapped to an undeclared scenario " DSCN_LIST[i])
  check_scenarios()
  check_links()
  check_groups()
  if (EXH) check_exhaustive()
  emit_dirs()
  emit_groups()
  for (i = 1; i <= NREG; i++) emit_row(i)
  emit_scenarios()
  emit_counters()
  if (nbad > 0) {
    printf "manifest: %d assertion(s) failed\n", nbad > "/dev/stderr"
    exit 1
  }
  printf "files=%d symlinks=%d indexable=%d dirs=%d empty_dirs=%d groups=%d bytes=%.0f\n", NFILES, NSYM, NIDX, NDIRS, NEM, NG, TOTB
}
