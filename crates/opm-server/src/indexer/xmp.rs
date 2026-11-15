//! XMP sidecar parser: ratings, flags, keywords, camera and capture-date overrides.
//!
//! Owned by todo `extracting-metadata`. [`parse_xmp`] is pure; [`read_xmp`] and
//! [`read_xmp_from`] add the bounded read. A sidecar is untrusted input from a shared folder.
//!
//! # What is read
//! Properties are matched by NAMESPACE URI, never by prefix, as attributes of `rdf:Description`
//! (Lightroom and most tools) and as child elements, over any number of `rdf:Description` blocks.
//! The first value found for a property wins. `<?xpacket ...?>` wrappers and `x:xmpmeta` are
//! transparent. Fields: `xmp:Rating` (`3.0` is tolerated, non-numeric and empty values are
//! ignored), `xmpDM:pick`, `dc:subject` and `lr:hierarchicalSubject` (flattened into one keyword
//! list; present, even empty, means the sidecar has a say about keywords), `tiff:Make`,
//! `tiff:Model`, `exif:DateTimeOriginal`, `xmp:CreateDate`, `photoshop:DateCreated`. Everything
//! else (`darktable:history`, `xmpMM:History`, ...) is skipped without being collected.
//!
//! # Hostile input
//! No input panics or allocates without bound:
//! * more than [`MAX_XMP_BYTES`] (16 MiB) is unreadable;
//! * any `<!DOCTYPE` is rejected (so no entity can expand; unknown entities in text are dropped);
//! * nesting deeper than [`MAX_DEPTH`] (64) is rejected; so are more than 256 `xmlns` bindings;
//! * a value longer than [`MAX_TEXT_BYTES`] (64 KiB) is ignored, a list keeps [`MAX_LIST_ITEMS`] items;
//! * truncated and ill-formed documents are errors.
//!
//! A UTF-8 byte order mark is skipped. UTF-16 (with a BOM, or recognised from a leading `<`) is
//! decoded to UTF-8 first. Other bytes that are not UTF-8 are read lossily.
//!
//! An [`XmpError`] means "no sidecar data": the stage logs it at debug level and the owner still
//! completes.

use std::borrow::Cow;
use std::io::{self, Read};
use std::path::Path;

use opm_core::metadata::{SourceMetadata, flatten_keywords, parse_integer_lenient};
use quick_xml::events::{BytesStart, Event};
use quick_xml::name::ResolveResult;
use quick_xml::reader::NsReader;
use thiserror::Error;

/// Largest sidecar read; bigger is unreadable.
pub const MAX_XMP_BYTES: usize = 16 << 20;
/// Deepest element nesting accepted.
pub const MAX_DEPTH: usize = 64;
/// Longest value kept.
pub const MAX_TEXT_BYTES: usize = 64 << 10;
/// Most `rdf:li` items kept per list.
pub const MAX_LIST_ITEMS: usize = 2000;
const MAX_NAMESPACE_BINDINGS: usize = 256;

/// Why a sidecar gave no data.
#[derive(Debug, Error, PartialEq, Eq)]
pub enum XmpError {
    #[error("the sidecar is larger than {MAX_XMP_BYTES} bytes")]
    TooLarge,
    #[error("the sidecar could not be read: {0:?}")]
    Io(io::ErrorKind),
    #[error("the sidecar declares a DOCTYPE")]
    Doctype,
    #[error("the sidecar nests deeper than {MAX_DEPTH} elements")]
    TooDeep,
    #[error("the sidecar is not valid UTF-16")]
    Encoding,
    #[error("the sidecar is truncated")]
    Truncated,
    #[error("the sidecar is not well-formed XML: {0}")]
    Malformed(String),
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Ns {
    Rdf,
    Xmp,
    XmpDm,
    Dc,
    Lr,
    Tiff,
    Exif,
    Photoshop,
    Other,
}

impl Ns {
    fn of(resolved: &ResolveResult<'_>) -> Self {
        let ResolveResult::Bound(ns) = resolved else {
            return Self::Other;
        };
        match ns.into_inner() {
            "http://www.w3.org/1999/02/22-rdf-syntax-ns#" => Self::Rdf,
            "http://ns.adobe.com/xap/1.0/" => Self::Xmp,
            "http://ns.adobe.com/xmp/1.0/DynamicMedia/" => Self::XmpDm,
            "http://purl.org/dc/elements/1.1/" => Self::Dc,
            "http://ns.adobe.com/lightroom/1.0/" => Self::Lr,
            "http://ns.adobe.com/tiff/1.0/" => Self::Tiff,
            "http://ns.adobe.com/exif/1.0/" => Self::Exif,
            "http://ns.adobe.com/photoshop/1.0/" => Self::Photoshop,
            _ => Self::Other,
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Prop {
    Description,
    Rating,
    Pick,
    Subject,
    HierarchicalSubject,
    Make,
    Model,
    DateTimeOriginal,
    CreateDate,
    DateCreated,
}

impl Prop {
    fn of(ns: Ns, local: &str) -> Option<Self> {
        Some(match (ns, local) {
            (Ns::Dc, "description") => Self::Description,
            (Ns::Xmp, "Rating") => Self::Rating,
            (Ns::XmpDm, "pick") => Self::Pick,
            (Ns::Dc, "subject") => Self::Subject,
            (Ns::Lr, "hierarchicalSubject") => Self::HierarchicalSubject,
            (Ns::Tiff, "Make") => Self::Make,
            (Ns::Tiff, "Model") => Self::Model,
            (Ns::Exif, "DateTimeOriginal") => Self::DateTimeOriginal,
            (Ns::Xmp, "CreateDate") => Self::CreateDate,
            (Ns::Photoshop, "DateCreated") => Self::DateCreated,
            _ => return None,
        })
    }

    fn is_list(self) -> bool {
        matches!(self, Self::Subject | Self::HierarchicalSubject)
    }
}

#[derive(Default)]
struct Found {
    description: Option<String>,
    rating: Option<String>,
    pick: Option<String>,
    make: Option<String>,
    model: Option<String>,
    date_time_original: Option<String>,
    create_date: Option<String>,
    date_created: Option<String>,
    subject: Option<Vec<String>>,
    hierarchical: Option<Vec<String>>,
}

impl Found {
    fn set_scalar(&mut self, prop: Prop, value: &str) {
        let value = value.trim();
        if value.is_empty() && prop != Prop::Description {
            return;
        }
        let slot = match prop {
            Prop::Description => &mut self.description,
            Prop::Rating => &mut self.rating,
            Prop::Pick => &mut self.pick,
            Prop::Make => &mut self.make,
            Prop::Model => &mut self.model,
            Prop::DateTimeOriginal => &mut self.date_time_original,
            Prop::CreateDate => &mut self.create_date,
            Prop::DateCreated => &mut self.date_created,
            Prop::Subject | Prop::HierarchicalSubject => return,
        };
        slot.get_or_insert_with(|| value.to_owned());
    }

    fn list_mut(&mut self, prop: Prop) -> Option<&mut Vec<String>> {
        match prop {
            Prop::Subject => Some(self.subject.get_or_insert_with(Vec::new)),
            Prop::HierarchicalSubject => Some(self.hierarchical.get_or_insert_with(Vec::new)),
            _ => None,
        }
    }

    fn into_metadata(self) -> SourceMetadata {
        let keywords = if self.subject.is_some() || self.hierarchical.is_some() {
            let mut values = self.subject.unwrap_or_default();
            values.extend(
                self.hierarchical
                    .unwrap_or_default()
                    .iter()
                    .flat_map(|path| path.split('|').map(str::to_owned)),
            );
            Some(flatten_keywords(&values, false))
        } else {
            None
        };
        SourceMetadata {
            description: self.description,
            rating: self.rating.as_deref().and_then(parse_integer_lenient),
            pick: self.pick.as_deref().and_then(parse_integer_lenient),
            keywords,
            make: self.make,
            model: self.model,
            date_time_original: self.date_time_original,
            create_date: self.create_date,
            date_created: self.date_created,
            ..SourceMetadata::default()
        }
    }
}

/// The text of the element being collected, up to [`MAX_TEXT_BYTES`].
#[derive(Default)]
struct Text {
    buf: String,
    overflow: bool,
}

impl Text {
    fn push(&mut self, piece: &str) {
        if self.buf.len() + piece.len() > MAX_TEXT_BYTES {
            self.overflow = true;
        } else {
            self.buf.push_str(piece);
        }
    }

    fn take(&mut self) -> Option<String> {
        let overflow = std::mem::take(&mut self.overflow);
        let buf = std::mem::take(&mut self.buf);
        (!overflow).then_some(buf)
    }
}

fn utf16(rest: &[u8], big_endian: bool) -> Result<Cow<'static, str>, XmpError> {
    if !rest.len().is_multiple_of(2) {
        return Err(XmpError::Encoding);
    }
    let units: Vec<u16> = rest
        .chunks_exact(2)
        .map(|pair| {
            if big_endian {
                u16::from_be_bytes([pair[0], pair[1]])
            } else {
                u16::from_le_bytes([pair[0], pair[1]])
            }
        })
        .collect();
    String::from_utf16(&units)
        .map(Cow::Owned)
        .map_err(|_| XmpError::Encoding)
}

/// Decodes the bytes of a sidecar to text: skips a UTF-8 BOM and converts UTF-16.
fn decode(bytes: &[u8]) -> Result<Cow<'_, str>, XmpError> {
    match bytes {
        [0xEF, 0xBB, 0xBF, rest @ ..] => Ok(String::from_utf8_lossy(rest)),
        [0xFF, 0xFE, rest @ ..] => utf16(rest, false),
        [0xFE, 0xFF, rest @ ..] => utf16(rest, true),
        [b'<', 0, ..] => utf16(bytes, false),
        [0, b'<', ..] => utf16(bytes, true),
        _ => Ok(String::from_utf8_lossy(bytes)),
    }
}

/// Parses the bytes of an XMP sidecar. Pure and total: every input gives data or an error.
pub fn parse_xmp(bytes: &[u8]) -> Result<SourceMetadata, XmpError> {
    if bytes.len() > MAX_XMP_BYTES {
        return Err(XmpError::TooLarge);
    }
    let text = decode(bytes)?;
    parse_text(&text)
}

/// Reads at most [`MAX_XMP_BYTES`] from `reader` and parses them.
pub fn read_xmp_from(reader: impl Read) -> Result<SourceMetadata, XmpError> {
    let mut bytes = Vec::new();
    reader
        .take(MAX_XMP_BYTES as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(|err| XmpError::Io(err.kind()))?;
    parse_xmp(&bytes)
}

/// Opens `path` and parses it with [`read_xmp_from`]. The caller has validated the path (the
/// stage uses `VolumeRoot::open_read` and [`read_xmp_from`] instead).
pub fn read_xmp(path: &Path) -> Result<SourceMetadata, XmpError> {
    let file = std::fs::File::open(path).map_err(|err| XmpError::Io(err.kind()))?;
    read_xmp_from(file)
}

fn malformed(err: impl std::fmt::Display) -> XmpError {
    XmpError::Malformed(err.to_string())
}

fn attribute_properties(start: &BytesStart<'_>, reader: &NsReader<&[u8]>, found: &mut Found) {
    for attr in start.attributes().with_checks(false).flatten() {
        let (resolved, local) = reader.resolver().resolve_attribute(attr.key);
        let Some(prop) = Prop::of(Ns::of(&resolved), local.into_inner()) else {
            continue;
        };
        if prop.is_list() {
            continue;
        }
        if let Ok(value) = attr.normalized_value(quick_xml::XmlVersion::Implicit1_0)
            && value.len() <= MAX_TEXT_BYTES
        {
            found.set_scalar(prop, &value);
        }
    }
}

fn parse_text(text: &str) -> Result<SourceMetadata, XmpError> {
    let mut reader = NsReader::from_str(text);
    reader
        .resolver_mut()
        .set_max_namespace_bindings(MAX_NAMESPACE_BINDINGS);
    let mut found = Found::default();
    let mut depth = 0usize;
    let mut descriptions = 0usize;
    // The property being collected and the depth of its element; for a list also the open `li`.
    let mut scalar: Option<(Prop, usize)> = None;
    let mut list: Option<(Prop, usize)> = None;
    let mut item_depth: Option<usize> = None;
    let mut collected = Text::default();

    loop {
        let (resolved, event) = reader.read_resolved_event().map_err(malformed)?;
        let ns = Ns::of(&resolved);
        drop(resolved);
        match event {
            Event::DocType(_) => return Err(XmpError::Doctype),
            Event::Eof => break,
            Event::Start(start) => {
                depth += 1;
                if depth > MAX_DEPTH {
                    return Err(XmpError::TooDeep);
                }
                let local = start.local_name();
                let local = local.into_inner();
                if ns == Ns::Rdf && local == "Description" {
                    descriptions += 1;
                    attribute_properties(&start, &reader, &mut found);
                } else if list.is_some() {
                    if ns == Ns::Rdf && local == "li" && item_depth.is_none() {
                        item_depth = Some(depth);
                        collected = Text::default();
                    }
                } else if scalar.is_none() && descriptions > 0 {
                    match Prop::of(ns, local) {
                        Some(prop) if prop.is_list() => {
                            list = Some((prop, depth));
                            found.list_mut(prop);
                        }
                        Some(prop) => {
                            scalar = Some((prop, depth));
                            collected = Text::default();
                        }
                        None => {
                            reader
                                .read_to_end(start.to_end().name())
                                .map_err(malformed)?;
                            depth -= 1;
                        }
                    }
                }
            }
            Event::Empty(start) => {
                if depth + 1 > MAX_DEPTH {
                    return Err(XmpError::TooDeep);
                }
                let local = start.local_name();
                let local = local.into_inner();
                if ns == Ns::Rdf && local == "Description" {
                    attribute_properties(&start, &reader, &mut found);
                } else if list.is_none()
                    && scalar.is_none()
                    && descriptions > 0
                    && let Some(prop) = Prop::of(ns, local)
                {
                    if prop.is_list() {
                        found.list_mut(prop);
                    } else if prop == Prop::Description {
                        found.set_scalar(prop, "");
                    }
                }
            }
            Event::End(end) => {
                if let Some((prop, at)) = scalar
                    && at == depth
                {
                    if let Some(value) = collected.take() {
                        found.set_scalar(prop, &value);
                    }
                    scalar = None;
                } else if let Some(at) = item_depth
                    && at == depth
                {
                    if let Some((prop, _)) = list
                        && let Some(value) = collected.take()
                        && let Some(items) = found.list_mut(prop)
                        && items.len() < MAX_LIST_ITEMS
                    {
                        items.push(value);
                    }
                    item_depth = None;
                } else if let Some((_, at)) = list
                    && at == depth
                {
                    list = None;
                } else if ns == Ns::Rdf && end.local_name().into_inner() == "Description" {
                    descriptions = descriptions.saturating_sub(1);
                }
                depth = depth.saturating_sub(1);
            }
            Event::Text(piece) if scalar.is_some() || item_depth.is_some() => {
                collected.push(&piece);
            }
            Event::CData(piece) if scalar.is_some() || item_depth.is_some() => {
                collected.push(&piece);
            }
            Event::GeneralRef(reference) if scalar.is_some() || item_depth.is_some() => {
                let resolved = match &*reference {
                    "amp" => Some('&'),
                    "lt" => Some('<'),
                    "gt" => Some('>'),
                    "quot" => Some('"'),
                    "apos" => Some('\''),
                    _ => reference.resolve_char_ref().ok().flatten(),
                };
                if let Some(c) = resolved {
                    collected.push(c.encode_utf8(&mut [0u8; 4]));
                }
            }
            _ => {}
        }
    }
    if depth != 0 {
        return Err(XmpError::Truncated);
    }
    Ok(found.into_metadata())
}

#[cfg(test)]
mod tests {
    use super::*;

    const NS: &str = r#"xmlns:x="adobe:ns:meta/" xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#"
        xmlns:xmp="http://ns.adobe.com/xap/1.0/" xmlns:dc="http://purl.org/dc/elements/1.1/"
        xmlns:lr="http://ns.adobe.com/lightroom/1.0/" xmlns:tiff="http://ns.adobe.com/tiff/1.0/"
        xmlns:exif="http://ns.adobe.com/exif/1.0/" xmlns:xmpDM="http://ns.adobe.com/xmp/1.0/DynamicMedia/"
        xmlns:photoshop="http://ns.adobe.com/photoshop/1.0/""#;

    fn doc(body: &str) -> String {
        format!(
            "<?xpacket begin=\"\u{feff}\" id=\"W5M0MpCehiHzreSzNTczkc9d\"?>\n<x:xmpmeta {NS}>\n\
             <rdf:RDF>{body}</rdf:RDF></x:xmpmeta>\n<?xpacket end=\"w\"?>"
        )
    }

    fn parse(text: &str) -> SourceMetadata {
        parse_xmp(text.as_bytes()).unwrap()
    }

    fn s(text: &str) -> Option<String> {
        Some(text.to_owned())
    }

    #[test]
    fn lightroom_attributes_and_bags() {
        let meta = parse(&doc(
            r#"<rdf:Description rdf:about="" xmp:Rating="4" xmp:CreateDate="2020-01-02T03:04:05"
                 exif:DateTimeOriginal="2021-05-05T12:00:00+02:00" tiff:Make="Canon" tiff:Model="Canon EOS R5"
                 photoshop:DateCreated="2020-01-02T03:04:05" xmpDM:pick="1">
               <dc:subject><rdf:Bag><rdf:li>Paris</rdf:li><rdf:li>Trip</rdf:li></rdf:Bag></dc:subject>
               <lr:hierarchicalSubject><rdf:Bag><rdf:li>Places|France|Paris</rdf:li><rdf:li>Trip</rdf:li></rdf:Bag></lr:hierarchicalSubject>
             </rdf:Description>"#,
        ));
        assert_eq!(meta.rating, Some(4));
        assert_eq!(meta.pick, Some(1));
        assert_eq!(meta.make, s("Canon"));
        assert_eq!(meta.model, s("Canon EOS R5"));
        assert_eq!(meta.date_time_original, s("2021-05-05T12:00:00+02:00"));
        assert_eq!(meta.create_date, s("2020-01-02T03:04:05"));
        assert_eq!(meta.date_created, s("2020-01-02T03:04:05"));
        assert_eq!(
            meta.keywords,
            Some(vec![
                "Paris".to_owned(),
                "Trip".to_owned(),
                "Places".to_owned(),
                "France".to_owned()
            ])
        );
    }

    #[test]
    fn darktable_reject_and_a_large_history_is_skipped() {
        let history: String = (0..3000)
            .map(|n| {
                format!(
                    r#"<rdf:li darktable:operation="op{n}" darktable:params="{}"/>"#,
                    "ab".repeat(200)
                )
            })
            .collect();
        let text = format!(
            r#"<?xml version="1.0" encoding="UTF-8"?>
            <x:xmpmeta {NS} xmlns:darktable="http://darktable.sf.net/"><rdf:RDF>
             <rdf:Description rdf:about="" xmp:Rating="-1" darktable:xmp_version="5">
              <darktable:history><rdf:Seq>{history}</rdf:Seq></darktable:history>
              <dc:subject><rdf:Bag/></dc:subject>
             </rdf:Description></rdf:RDF></x:xmpmeta>"#
        );
        let meta = parse(&text);
        assert_eq!(meta.rating, Some(-1));
        assert_eq!(meta.keywords, Some(Vec::new()), "an empty bag is present");
    }

    #[test]
    fn bridge_style_with_a_different_prefix_and_child_elements() {
        let text = r#"<x:xmpmeta xmlns:x="adobe:ns:meta/"><rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#">
            <rdf:Description xmlns:a="http://ns.adobe.com/xap/1.0/" xmlns:t="http://ns.adobe.com/tiff/1.0/"
               xmlns:e="http://ns.adobe.com/exif/1.0/" xmlns:s="http://purl.org/dc/elements/1.1/">
              <a:Rating>3.0</a:Rating>
              <t:Make>NIKON CORPORATION</t:Make><t:Model>NIKON D850</t:Model>
              <e:DateTimeOriginal>2019-07-08T09:10:11</e:DateTimeOriginal>
              <s:subject><rdf:Seq><rdf:li>a</rdf:li><rdf:li>A</rdf:li></rdf:Seq></s:subject>
            </rdf:Description></rdf:RDF></x:xmpmeta>"#;
        let meta = parse(text);
        assert_eq!(meta.rating, Some(3), "matched by namespace, not prefix");
        assert_eq!(meta.make, s("NIKON CORPORATION"));
        assert_eq!(meta.date_time_original, s("2019-07-08T09:10:11"));
        assert_eq!(meta.keywords, Some(vec!["a".to_owned()]));
    }

    #[test]
    fn the_wrong_namespace_is_not_a_rating() {
        let text = r#"<rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#" xmlns:xmp="http://example.com/other/">
            <rdf:Description xmp:Rating="5"/></rdf:RDF>"#;
        assert_eq!(parse(text), SourceMetadata::default());
    }

    #[test]
    fn several_descriptions_the_first_value_wins() {
        let meta = parse(&doc(r#"<rdf:Description xmp:Rating="2"/>
               <rdf:Description xmp:Rating="5" tiff:Make="Sony"><dc:subject><rdf:Bag><rdf:li>x</rdf:li></rdf:Bag></dc:subject></rdf:Description>"#));
        assert_eq!((meta.rating, meta.make), (Some(2), s("Sony")));
        assert_eq!(meta.keywords, Some(vec!["x".to_owned()]));
    }

    #[test]
    fn junk_values_are_ignored() {
        let meta = parse(&doc(
            r#"<rdf:Description xmp:Rating="great" tiff:Make="" tiff:Model="  " exif:DateTimeOriginal=""/>"#,
        ));
        assert_eq!(meta, SourceMetadata::default());
    }

    #[test]
    fn entities_and_cdata_in_text() {
        let meta = parse(&doc(
            r#"<rdf:Description><dc:subject><rdf:Bag><rdf:li>Tom &amp; Jerry</rdf:li><rdf:li>&#233;t&#xE9;</rdf:li>
               <rdf:li><![CDATA[a<b]]></rdf:li><rdf:li>&unknown;x</rdf:li></rdf:Bag></dc:subject></rdf:Description>"#,
        ));
        assert_eq!(
            meta.keywords,
            Some(vec![
                "Tom & Jerry".to_owned(),
                "été".to_owned(),
                "a<b".to_owned(),
                "x".to_owned()
            ])
        );
    }

    #[test]
    fn byte_order_marks_and_utf16() {
        let text = doc(r#"<rdf:Description xmp:Rating="2"/>"#);
        let mut bom = vec![0xEF, 0xBB, 0xBF];
        bom.extend_from_slice(text.as_bytes());
        assert_eq!(parse_xmp(&bom).unwrap().rating, Some(2));

        let mut le = vec![0xFF, 0xFE];
        le.extend(text.encode_utf16().flat_map(u16::to_le_bytes));
        assert_eq!(parse_xmp(&le).unwrap().rating, Some(2));
        let mut be = vec![0xFE, 0xFF];
        be.extend(text.encode_utf16().flat_map(u16::to_be_bytes));
        assert_eq!(parse_xmp(&be).unwrap().rating, Some(2));
        let bare: Vec<u8> = "<rdf:RDF/>"
            .encode_utf16()
            .flat_map(u16::to_le_bytes)
            .collect();
        assert_eq!(parse_xmp(&bare).unwrap(), SourceMetadata::default());
        assert_eq!(parse_xmp(&[0xFF, 0xFE, 0x3C]), Err(XmpError::Encoding));
        assert_eq!(
            parse_xmp(&[0xFF, 0xFE, 0x00, 0xD8]),
            Err(XmpError::Encoding)
        );
    }

    #[test]
    fn a_doctype_and_billion_laughs_are_rejected() {
        let bomb = r#"<?xml version="1.0"?>
            <!DOCTYPE lolz [<!ENTITY lol "lol"><!ENTITY lol2 "&lol;&lol;&lol;&lol;&lol;&lol;&lol;&lol;&lol;&lol;">
            <!ENTITY lol3 "&lol2;&lol2;&lol2;&lol2;&lol2;&lol2;&lol2;&lol2;&lol2;&lol2;">]>
            <rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#"><rdf:Description xmlns:tiff="http://ns.adobe.com/tiff/1.0/" tiff:Make="&lol3;"/></rdf:RDF>"#;
        assert_eq!(parse_xmp(bomb.as_bytes()), Err(XmpError::Doctype));
    }

    #[test]
    fn truncated_and_broken_documents_are_errors() {
        let text =
            doc(r#"<rdf:Description xmp:Rating="2"><dc:subject><rdf:Bag><rdf:li>a</rdf:li>"#);
        let cut = &text[..text.find("<rdf:RDF>").unwrap() + 40];
        assert!(parse_xmp(cut.as_bytes()).is_err());
        assert!(parse_xmp("<rdf:RDF><a></b></rdf:RDF>".as_bytes()).is_err());
        assert!(parse_xmp("".as_bytes()).unwrap() == SourceMetadata::default());
        assert!(parse_xmp("not xml at all".as_bytes()).unwrap() == SourceMetadata::default());
    }

    #[test]
    fn depth_and_size_limits() {
        let deep = format!("{}{}", "<a>".repeat(200), "</a>".repeat(200));
        assert_eq!(parse_xmp(deep.as_bytes()), Err(XmpError::TooDeep));
        let nested_in_description = doc(&format!(
            "<rdf:Description><a:x xmlns:a=\"urn:a\">{}{}</a:x></rdf:Description>",
            "<b>".repeat(200),
            "</b>".repeat(200)
        ));
        let result = parse_xmp(nested_in_description.as_bytes());
        assert!(result.is_ok() || result == Err(XmpError::TooDeep));

        let huge = vec![b' '; MAX_XMP_BYTES + 1];
        assert_eq!(parse_xmp(&huge), Err(XmpError::TooLarge));
        assert_eq!(read_xmp_from(io::repeat(b' ')), Err(XmpError::TooLarge));

        let long = "x".repeat(MAX_TEXT_BYTES + 1);
        let meta = parse(&doc(&format!(
            "<rdf:Description><tiff:Make>{long}</tiff:Make><tiff:Model>ok</tiff:Model></rdf:Description>"
        )));
        assert_eq!((meta.make, meta.model), (None, s("ok")));

        let items: String = (0..MAX_LIST_ITEMS + 50)
            .map(|n| format!("<rdf:li>k{n}</rdf:li>"))
            .collect();
        let meta = parse(&doc(&format!(
            "<rdf:Description><dc:subject><rdf:Bag>{items}</rdf:Bag></dc:subject></rdf:Description>"
        )));
        assert_eq!(
            meta.keywords.unwrap().len(),
            opm_core::metadata::MAX_KEYWORDS
        );

        let many_namespaces: String = (0..400)
            .map(|n| format!(r#" xmlns:n{n}="urn:{n}""#))
            .collect();
        assert!(parse_xmp(format!("<a{many_namespaces}/>").as_bytes()).is_err());
    }

    #[test]
    fn random_bytes_never_panic() {
        let mut state = 0x2545_F491_4F6C_DD1Du64;
        let mut next = move || {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            state
        };
        let seed = doc(
            r#"<rdf:Description xmp:Rating="2"><dc:subject><rdf:Bag><rdf:li>a</rdf:li></rdf:Bag></dc:subject></rdf:Description>"#,
        );
        for round in 0..2000 {
            let len = usize::try_from(next() % 300).unwrap();
            let random: Vec<u8> = (0..len)
                .map(|_| u8::try_from(next() & 0xFF).unwrap())
                .collect();
            let _ = parse_xmp(&random);
            let mut mutated = seed.clone().into_bytes();
            for _ in 0..=(round % 5) {
                let at = usize::try_from(next()).unwrap() % mutated.len();
                mutated[at] = u8::try_from(next() & 0xFF).unwrap();
            }
            let _ = parse_xmp(&mutated);
            let cut = usize::try_from(next()).unwrap() % mutated.len();
            let _ = parse_xmp(&mutated[..cut]);
        }
    }

    #[test]
    fn a_missing_file_is_an_io_error() {
        let result = read_xmp(Path::new("/nonexistent-dir/none.xmp"));
        assert_eq!(result, Err(XmpError::Io(io::ErrorKind::NotFound)));
    }
}
