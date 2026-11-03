//! Minimal PDF 1.7 object serializer.
//!
//! Objects are allocated up front (so cross references can be written before
//! the target object body exists), filled in any order, and serialized in id
//! order at the end, which is when the xref table offsets become known.

use std::fmt::Write as _;

use flate2::write::ZlibEncoder;
use flate2::Compression;
use std::io::Write as _;

/// An indirect object reference, e.g. `4 0 R`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Ref(pub usize);

impl std::fmt::Display for Ref {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} 0 R", self.0)
    }
}

#[derive(Default)]
pub struct Pdf {
    objects: Vec<Option<Vec<u8>>>,
}

impl Pdf {
    pub fn new() -> Self {
        Self::default()
    }

    /// Reserves an object id that can be referenced before it is filled in.
    pub fn alloc(&mut self) -> Ref {
        self.objects.push(None);
        Ref(self.objects.len())
    }

    /// Stores a plain (non-stream) object body, e.g. a dictionary.
    pub fn set(&mut self, r: Ref, body: impl AsRef<[u8]>) {
        self.objects[r.0 - 1] = Some(body.as_ref().to_vec());
    }

    /// Stores a stream object. `extra_dict` holds entries beyond `/Length`
    /// and `/Filter` (both are written automatically).
    pub fn set_stream(&mut self, r: Ref, extra_dict: &str, data: &[u8], compress: bool) {
        let payload = if compress {
            deflate(data)
        } else {
            data.to_vec()
        };
        let mut body = Vec::with_capacity(payload.len() + 128);
        body.extend_from_slice(b"<< /Length ");
        body.extend_from_slice(payload.len().to_string().as_bytes());
        if compress {
            body.extend_from_slice(b" /Filter /FlateDecode");
        }
        if !extra_dict.is_empty() {
            body.push(b' ');
            body.extend_from_slice(extra_dict.as_bytes());
        }
        body.extend_from_slice(b" >>\nstream\n");
        body.extend_from_slice(&payload);
        body.extend_from_slice(b"\nendstream");
        self.set(r, body);
    }

    /// Serializes the document. Panics if an allocated object was never filled.
    pub fn finish(self, root: Ref, info: Option<Ref>) -> Vec<u8> {
        let mut out: Vec<u8> = Vec::with_capacity(64 * 1024);
        // Binary comment marks the file as containing binary data (PDF 1.7, 7.5.2).
        out.extend_from_slice(b"%PDF-1.7\n%\xE2\xE3\xCF\xD3\n");

        let mut offsets = Vec::with_capacity(self.objects.len());
        for (idx, obj) in self.objects.iter().enumerate() {
            let body = obj
                .as_ref()
                .unwrap_or_else(|| panic!("pdf object {} was allocated but never set", idx + 1));
            offsets.push(out.len());
            out.extend_from_slice(format!("{} 0 obj\n", idx + 1).as_bytes());
            out.extend_from_slice(body);
            out.extend_from_slice(b"\nendobj\n");
        }

        let startxref = out.len();
        let size = self.objects.len() + 1;
        out.extend_from_slice(format!("xref\n0 {size}\n").as_bytes());
        // Each entry is exactly 20 bytes, including the trailing two-byte EOL.
        out.extend_from_slice(b"0000000000 65535 f \n");
        for off in &offsets {
            out.extend_from_slice(format!("{off:010} 00000 n \n").as_bytes());
        }

        let mut trailer = format!("trailer\n<< /Size {size} /Root {root}");
        if let Some(info) = info {
            let _ = write!(trailer, " /Info {info}");
        }
        trailer.push_str(" >>\n");
        out.extend_from_slice(trailer.as_bytes());
        out.extend_from_slice(format!("startxref\n{startxref}\n%%EOF\n").as_bytes());
        out
    }
}

fn deflate(data: &[u8]) -> Vec<u8> {
    let mut enc = ZlibEncoder::new(Vec::new(), Compression::default());
    enc.write_all(data).expect("writing to a Vec cannot fail");
    enc.finish().expect("flushing to a Vec cannot fail")
}

/// Formats a number for PDF content streams: fixed precision, no exponent,
/// and no `-0` (some strict consumers dislike it).
pub fn num(v: f64) -> String {
    let v = if v == 0.0 { 0.0 } else { v };
    let mut s = format!("{v:.3}");
    if s.contains('.') {
        while s.ends_with('0') {
            s.pop();
        }
        if s.ends_with('.') {
            s.pop();
        }
    }
    if s == "-0" {
        s = "0".into();
    }
    s
}

/// Encodes a string as a PDF text string: UTF-16BE with a byte order mark,
/// written as a hex string so no escaping is needed.
pub fn text_string(s: &str) -> String {
    let mut out = String::from("<FEFF");
    for unit in s.encode_utf16() {
        let _ = write!(out, "{unit:04X}");
    }
    out.push('>');
    out
}

/// Encodes a glyph-id run as a hex string for use with `Tj` under Identity-H.
pub fn glyph_string(glyphs: &[u16]) -> String {
    let mut out = String::with_capacity(glyphs.len() * 4 + 2);
    out.push('<');
    for g in glyphs {
        let _ = write!(out, "{g:04X}");
    }
    out.push('>');
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn number_formatting_is_compact() {
        assert_eq!(num(0.0), "0");
        assert_eq!(num(-0.0), "0");
        assert_eq!(num(12.0), "12");
        assert_eq!(num(595.2756), "595.276");
        assert_eq!(num(35.43307), "35.433");
    }

    #[test]
    fn strings_encode_as_utf16be_hex() {
        assert_eq!(text_string("AB"), "<FEFF00410042>");
        assert_eq!(text_string("Ж"), "<FEFF0416>");
        assert_eq!(glyph_string(&[1, 0xABCD]), "<0001ABCD>");
    }

    #[test]
    fn xref_entries_are_twenty_bytes_and_offsets_point_at_objects() {
        let mut pdf = Pdf::new();
        let root = pdf.alloc();
        let pages = pdf.alloc();
        pdf.set(root, format!("<< /Type /Catalog /Pages {pages} >>"));
        pdf.set(pages, "<< /Type /Pages /Kids [] /Count 0 >>");
        let bytes = pdf.finish(root, None);

        // The file contains a binary comment, so all offsets are byte offsets.
        let find = |needle: &[u8]| {
            bytes
                .windows(needle.len())
                .position(|w| w == needle)
                .unwrap_or_else(|| panic!("missing {:?}", String::from_utf8_lossy(needle)))
        };
        let xref_at = find(b"xref\n0 3\n");
        let table = &bytes[xref_at + b"xref\n0 3\n".len()..];
        for i in 0..3 {
            let entry = &table[i * 20..(i + 1) * 20];
            assert_eq!(entry.len(), 20);
            assert!(
                entry.ends_with(b" \n"),
                "{:?}",
                String::from_utf8_lossy(entry)
            );
        }
        assert!(table[..20].starts_with(b"0000000000 65535 f"));

        // The offset recorded for object 1 must land exactly on "1 0 obj".
        let entry = std::str::from_utf8(&table[20..40]).unwrap();
        let off: usize = entry.split(' ').next().unwrap().parse().unwrap();
        assert!(bytes[off..].starts_with(b"1 0 obj"));

        let sx = find(b"startxref\n") + b"startxref\n".len();
        let declared: usize = std::str::from_utf8(&bytes[sx..])
            .unwrap()
            .lines()
            .next()
            .unwrap()
            .parse()
            .unwrap();
        assert_eq!(declared, xref_at);
    }

    #[test]
    #[should_panic(expected = "never set")]
    fn unset_objects_are_caught() {
        let mut pdf = Pdf::new();
        let root = pdf.alloc();
        pdf.finish(root, None);
    }
}
