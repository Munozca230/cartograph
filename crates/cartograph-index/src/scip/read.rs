//! Streaming decoder: a SCIP file → [`ScipData`], one document at a time.
//!
//! An `Index` message is a sequence of top-level fields (1 = metadata, 2 = one document,
//! 3 = one external symbol). The decoder reads them one by one from a buffered reader, so
//! only one decoded `Document` is in memory at a time; each is reduced at once to the few
//! numbers N3 needs (definitions, references, lines) and dropped. Text, documentation,
//! signatures and diagnostics are never kept.

use super::{NO_END, ScipData, ScipDef, ScipDoc};
use cartograph_core::SymbolKind;
use protobuf::rt::WireType;
use protobuf::{CodedInputStream, Message};
use scip::types::{Document, Metadata, Occurrence, descriptor, occurrence, symbol_information};
use std::collections::HashMap;
use std::io::Read;
use std::path::Path;

/// Largest SCIP file read. Decoding is streamed, so this bounds time, not memory.
pub const MAX_FILE_BYTES: u64 = 4 << 30;
/// Largest single document (one source file's occurrences).
const MAX_DOC_BYTES: u64 = 512 << 20;

/// `SymbolRole` bits (scip.proto).
const ROLE_DEFINITION: i32 = 1;
const ROLE_IMPORT: i32 = 2;
const ROLE_FORWARD_DEFINITION: i32 = 64;

/// A reader that hashes what passes through it.
struct Hashing<R> {
    inner: R,
    hasher: blake3::Hasher,
}

impl<R: Read> Read for Hashing<R> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        let n = self.inner.read(buf)?;
        self.hasher.update(&buf[..n]);
        Ok(n)
    }
}

/// BLAKE3 hex digest of the file at `path`, streamed.
pub fn hash_file(path: &Path) -> Result<String, String> {
    let f = std::fs::File::open(path).map_err(|e| format!("cannot open: {e}"))?;
    let mut h = Hashing {
        inner: std::io::BufReader::with_capacity(1 << 16, f),
        hasher: blake3::Hasher::new(),
    };
    std::io::copy(&mut h, &mut std::io::sink()).map_err(|e| format!("cannot read: {e}"))?;
    Ok(h.hasher.finalize().to_hex().to_string())
}

/// Decode the SCIP file at `path`; also returns its BLAKE3 hex digest. Every failure (a
/// missing file, a file over [`MAX_FILE_BYTES`], a corrupt or truncated message, a file
/// that is not a SCIP index, even a panic in the protobuf runtime) is an error message.
pub fn read(path: &Path) -> Result<(ScipData, String), String> {
    let size = std::fs::metadata(path)
        .map_err(|e| format!("cannot read: {e}"))?
        .len();
    if size > MAX_FILE_BYTES {
        return Err(format!(
            "{size} bytes, over the {} MiB limit",
            MAX_FILE_BYTES >> 20
        ));
    }
    let f = std::fs::File::open(path).map_err(|e| format!("cannot open: {e}"))?;
    let mut reader = Hashing {
        inner: std::io::BufReader::with_capacity(1 << 16, f),
        hasher: blake3::Hasher::new(),
    };
    let decoded = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| decode(&mut reader)));
    let data = match decoded {
        Ok(r) => r?,
        Err(_) => return Err("the protobuf decoder failed on this file".to_string()),
    };
    Ok((data, reader.hasher.finalize().to_hex().to_string()))
}

/// Decode an `Index` from `r` (read to the end).
fn decode(r: &mut dyn Read) -> Result<ScipData, String> {
    let bad = |e: protobuf::Error| format!("not a valid SCIP index: {e}");
    let mut is = CodedInputStream::new(r);
    let mut data = ScipData::default();
    let mut symbols: HashMap<String, u32> = HashMap::new();
    let (mut metadata, mut documents) = (false, 0u64);
    while let Some(tag) = is.read_raw_tag_or_eof().map_err(bad)? {
        let (field, wire) = (tag >> 3, tag & 7);
        match (field, wire) {
            (1, 2) => {
                let m: Metadata = read_bounded(&mut is).map_err(bad)?;
                if let Some(t) = m.tool_info.as_ref() {
                    data.tool = format!("{} {}", t.name, t.version).trim().to_string();
                }
                data.project_root = m.project_root;
                metadata = true;
            }
            (2, 2) => {
                let d: Document = read_bounded(&mut is).map_err(bad)?;
                compact(&mut data, &mut symbols, d);
                documents += 1;
            }
            _ => {
                let w = WireType::new(wire)
                    .filter(|w| !matches!(w, WireType::StartGroup | WireType::EndGroup))
                    .ok_or_else(|| format!("not a valid SCIP index: wire type {wire}"))?;
                is.skip_field(w).map_err(bad)?;
            }
        }
    }
    if !metadata && documents == 0 {
        return Err("not a SCIP index (no metadata and no documents)".to_string());
    }
    Ok(data)
}

/// One length-delimited message, refused when larger than [`MAX_DOC_BYTES`].
fn read_bounded<M: Message>(is: &mut CodedInputStream<'_>) -> protobuf::Result<M> {
    let len = is.read_raw_varint64()?;
    if len > MAX_DOC_BYTES {
        return Err(protobuf::Error::from(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!("a message of {len} bytes (limit {MAX_DOC_BYTES})"),
        )));
    }
    let old = is.push_limit(len)?;
    let mut m = M::new();
    m.merge_from(is)?;
    is.pop_limit(old);
    Ok(m)
}

fn intern(data: &mut ScipData, symbols: &mut HashMap<String, u32>, s: &str) -> u32 {
    if let Some(&i) = symbols.get(s) {
        return i;
    }
    let i = data.symbols.len() as u32;
    data.symbols.push(s.to_string());
    symbols.insert(s.to_string(), i);
    i
}

/// `relative_path` with forward slashes and no `./`; `None` for an absolute path or one
/// that climbs out (`..`).
pub(super) fn clean_path(p: &str) -> Option<String> {
    let p = p.replace('\\', "/");
    if p.starts_with('/') || p.contains(':') {
        return None;
    }
    let mut parts = Vec::new();
    for c in p.split('/') {
        match c {
            "" | "." => {}
            ".." => return None,
            c => parts.push(c),
        }
    }
    (!parts.is_empty()).then(|| parts.join("/"))
}

/// Reduce one document to definitions and references of global symbols.
fn compact(data: &mut ScipData, symbols: &mut HashMap<String, u32>, d: Document) {
    data.documents += 1;
    let Some(path) = clean_path(&d.relative_path) else {
        data.occurrences += d.occurrences.len() as u64;
        data.bad_paths += 1;
        return;
    };
    let kinds: HashMap<&str, symbol_information::Kind> = d
        .symbols
        .iter()
        .filter_map(|s| Some((s.symbol.as_str(), s.kind.enum_value().ok()?)))
        .collect();
    let mut doc = ScipDoc {
        path,
        encoding: u8::try_from(d.position_encoding.value()).unwrap_or(0),
        ..ScipDoc::default()
    };
    let mut defined: std::collections::HashSet<u32> = std::collections::HashSet::new();
    for occ in &d.occurrences {
        data.occurrences += 1;
        if occ.symbol.is_empty() || scip::symbol::is_local_symbol(&occ.symbol) {
            continue;
        }
        let Some((line, end)) = lines(occ) else {
            data.bad_ranges += 1;
            continue;
        };
        let sym = intern(data, symbols, &occ.symbol);
        let roles = occ.symbol_roles;
        if roles & ROLE_DEFINITION != 0 {
            let (name, container, kind) = describe(&occ.symbol, kinds.get(occ.symbol.as_str()));
            defined.insert(sym);
            doc.defs.push(ScipDef {
                sym,
                line,
                end: enclosing_end(occ).unwrap_or(end).max(line),
                name,
                container,
                kind,
            });
        } else if roles & ROLE_FORWARD_DEFINITION != 0 {
            // A declaration (a C++ prototype): neither the definition nor a use.
        } else if roles & ROLE_IMPORT != 0 {
            doc.imports.push([sym, line]);
        } else {
            doc.refs.push([sym, line, end_char(occ).unwrap_or(NO_END)]);
        }
    }
    // Symbols the document declares without a definition occurrence: their file is known.
    for s in &d.symbols {
        if s.symbol.is_empty() || scip::symbol::is_local_symbol(&s.symbol) {
            continue;
        }
        let sym = intern(data, symbols, &s.symbol);
        if !defined.contains(&sym) {
            doc.declared.push(sym);
        }
    }
    data.docs.push(doc);
}

/// 0-based start and end lines of an occurrence's range (typed or legacy encoding).
fn lines(o: &Occurrence) -> Option<(u32, u32)> {
    let (start, end) = match &o.typed_range {
        Some(occurrence::Typed_range::SingleLineRange(r)) => (r.line, r.line),
        Some(occurrence::Typed_range::MultiLineRange(r)) => (r.start_line, r.end_line),
        Some(_) => return None,
        None => legacy(&o.range)?,
    };
    Some((
        u32::try_from(start).ok()?,
        u32::try_from(end.max(start)).ok()?,
    ))
}

/// The character where a single-line occurrence's range ends; `None` for a multi-line one.
fn end_char(o: &Occurrence) -> Option<u32> {
    let end = match &o.typed_range {
        Some(occurrence::Typed_range::SingleLineRange(r)) => r.end_character,
        Some(occurrence::Typed_range::MultiLineRange(r)) if r.start_line == r.end_line => {
            r.end_character
        }
        Some(_) => return None,
        None => match o.range.as_slice() {
            [_, _, e] => *e,
            [sl, _, el, e] if sl == el => *e,
            _ => return None,
        },
    };
    u32::try_from(end).ok()
}

fn legacy(r: &[i32]) -> Option<(i32, i32)> {
    match r {
        [l, _, _] => Some((*l, *l)),
        [sl, _, el, _] => Some((*sl, *el)),
        _ => None,
    }
}

/// 0-based last line of the occurrence's enclosing range (the whole definition), if given.
fn enclosing_end(o: &Occurrence) -> Option<u32> {
    let end = match &o.typed_enclosing_range {
        Some(occurrence::Typed_enclosing_range::SingleLineEnclosingRange(r)) => r.line,
        Some(occurrence::Typed_enclosing_range::MultiLineEnclosingRange(r)) => r.end_line,
        Some(_) => return None,
        None => legacy(&o.enclosing_range)?.1,
    };
    u32::try_from(end).ok()
}

/// Name, container and kind of a global symbol, from its descriptors and the document's
/// `SymbolInformation.kind` (when given). The kind is `None` for what N2 does not model
/// (fields, variables, parameters, namespaces, macros).
fn describe(
    symbol: &str,
    info: Option<&symbol_information::Kind>,
) -> (String, Option<String>, Option<SymbolKind>) {
    use descriptor::Suffix;
    let Ok(parsed) = scip::symbol::parse_symbol(symbol) else {
        return (String::new(), None, None);
    };
    let descs = &parsed.descriptors;
    let Some(last) = descs.last() else {
        return (String::new(), None, None);
    };
    let suffix = last.suffix.enum_value().ok();
    let container = descs
        .len()
        .checked_sub(2)
        .and_then(|i| descs.get(i))
        .filter(|d| d.suffix.enum_value().ok() == Some(Suffix::Type))
        .map(|d| d.name.clone())
        .filter(|n| !n.is_empty());
    let callable = |c: &Option<String>| {
        if c.is_some() {
            SymbolKind::Method
        } else {
            SymbolKind::Function
        }
    };
    let by_info = info.and_then(|k| {
        use symbol_information::Kind as K;
        Some(match k {
            K::Function => callable(&container),
            K::Method
            | K::AbstractMethod
            | K::StaticMethod
            | K::TraitMethod
            | K::ProtocolMethod
            | K::PureVirtualMethod
            | K::TypeClassMethod
            | K::SingletonMethod
            | K::MethodSpecification
            | K::Constructor
            | K::Getter
            | K::Setter
            | K::Accessor => callable(&container),
            K::Class | K::SingletonClass | K::Object => SymbolKind::Class,
            K::Interface | K::Protocol => SymbolKind::Interface,
            K::Struct | K::Union => SymbolKind::Struct,
            K::Enum => SymbolKind::Enum,
            K::Trait | K::TypeClass | K::Mixin => SymbolKind::Trait,
            K::TypeAlias | K::Type => SymbolKind::Type,
            K::Constant if container.is_none() => SymbolKind::Const,
            _ => return None,
        })
    });
    let kind = match (info, by_info) {
        (_, Some(k)) => Some(k),
        // A kind N2 does not model (a field, a variable…).
        (Some(k), None) if *k != symbol_information::Kind::UnspecifiedKind => None,
        _ => match suffix {
            Some(Suffix::Method) => Some(callable(&container)),
            Some(Suffix::Type) => Some(SymbolKind::Class),
            _ => None,
        },
    };
    (last.name.clone(), container, kind)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paths_are_cleaned_and_escapes_refused() {
        assert_eq!(clean_path("./src\\a.ts").as_deref(), Some("src/a.ts"));
        assert_eq!(clean_path("src//b/./c.rs").as_deref(), Some("src/b/c.rs"));
        assert_eq!(clean_path("../x.ts"), None);
        assert_eq!(clean_path("a/../../x.ts"), None);
        assert_eq!(clean_path("/abs/x.ts"), None);
        assert_eq!(clean_path("C:/x.ts"), None);
        assert_eq!(clean_path(""), None);
    }

    #[test]
    fn symbols_are_described_from_descriptors() {
        let (n, c, k) = describe(
            "scip-typescript npm app 1.0.0 src/`a.ts`/Store#save().",
            None,
        );
        assert_eq!(
            (n.as_str(), c.as_deref(), k),
            ("save", Some("Store"), Some(SymbolKind::Method))
        );
        let (n, c, k) = describe("scip-typescript npm app 1.0.0 src/`a.ts`/helper().", None);
        assert_eq!(
            (n.as_str(), c, k),
            ("helper", None, Some(SymbolKind::Function))
        );
        let (_, _, k) = describe(
            "scip-typescript npm app 1.0.0 src/`a.ts`/Store#items.",
            None,
        );
        assert_eq!(k, None);
        let (_, _, k) = describe(
            "scip-typescript npm app 1.0.0 src/`a.ts`/Shape#",
            Some(&symbol_information::Kind::Interface),
        );
        assert_eq!(k, Some(SymbolKind::Interface));
        // A field described as such stays unmodelled even with a method-like name.
        let (_, _, k) = describe(
            "scip-typescript npm app 1.0.0 src/`a.ts`/Store#count.",
            Some(&symbol_information::Kind::Field),
        );
        assert_eq!(k, None);
        assert_eq!(describe("garbage", None).2, None);
    }

    #[test]
    fn garbage_is_an_error_not_a_panic() {
        for bytes in [
            &b""[..],
            &b"hello, world"[..],
            &[0x12, 0xff, 0xff, 0xff, 0xff, 0x0f][..],
            &[0x0a, 0x05, 0x01][..],
            &[0xff; 64][..],
        ] {
            let mut r: &[u8] = bytes;
            assert!(decode(&mut r).is_err(), "{bytes:?}");
        }
    }
}
