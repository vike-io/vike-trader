//! A strategy file's COMPATIBILITY RECORD (`ABI_VERSION` 15; decision 0296's step 1, the owner's "a
//! compatibility record inside the .so, checked before loading"): what the artifact was built against,
//! read from the FILE by the host before `dlopen` runs any of its code.
//!
//! **Where it lives.** In the ELF section [`IDENTITY_SECTION`] of the artifact itself, never in a file beside
//! it: the sandboxed child's jail grants the artifact alone (`vike_sandbox::strategy_process_policy`), and a
//! record kept apart from its bytes can drift from them (a copied `.so`, a prune that takes one of the
//! pair). Each cdylib template bakes it as a `#[used]` static in that section, [`RECORD`] as the guest
//! compiled it, and exports `vike_strategy_identity` returning its address, so the linker's section garbage
//! collection keeps it.
//!
//! **What it says** ([`record`]'s layout, [`RECORD_LEN`] bytes): a magic and format number, the interface
//! version (`crate::abi::ABI_VERSION`), the layout of the rows read in place (`crate::abi::layout`), the
//! strategy boundary's hash (`crate::sdk::BOUNDARY_HASH`) and the toolchain fingerprint
//! (`crate::fingerprint::FINGERPRINT`). The loader compares each with its own (`crate::loader`'s
//! `open_and_handshake`) and refuses a mismatch in words that say to build the file again.
//!
//! ⚠ **The parser is a trust boundary**: the file is a user's build. [`read`] reads only what it needs,
//! bounded, and refuses a header that is not 64-bit little-endian ELF, a truncated one, an oversized one
//! (more than [`MAX_SECTIONS`] sections, a name table above [`MAX_NAME_TABLE`]), sections that overlap one
//! another or the headers, a missing or doubled record section, and a record of the wrong size or shape.
//! Safe code throughout: no `mmap`, no pointer read.

use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;

/// The ELF section a strategy file's record lives in. The templates spell it in their `link_section`
/// attribute (an attribute takes no constant); a test holds the two to it.
pub const IDENTITY_SECTION: &str = ".vike_identity";

/// The record's first eight bytes: its name and its format number (1).
const MAGIC: [u8; 8] = *b"VIKEID\x00\x01";

/// The bytes before the fingerprint: the magic, the version (4), the layout (8), the boundary hash (16
/// hex digits) and the fingerprint's length (2).
const FIXED_LEN: usize = 8 + 4 + 8 + 16 + 2;

/// The most fingerprint bytes a record holds; the rest of the room is zeros.
pub const FINGERPRINT_CAPACITY: usize = 218;

/// A record's size, exactly: a section of any other size is refused.
pub const RECORD_LEN: usize = FIXED_LEN + FINGERPRINT_CAPACITY;

/// The most section headers [`read`] accepts (an ordinary artifact has about forty).
pub const MAX_SECTIONS: u64 = 4096;

/// The largest section-name table [`read`] accepts, in bytes (an ordinary artifact's is under one KiB).
pub const MAX_NAME_TABLE: u64 = 64 * 1024;

/// The record THIS build of the crate writes and expects: its own version, layout, fingerprint and boundary.
/// A strategy file's template bakes the guest's copy into [`IDENTITY_SECTION`]; the loader compares the
/// fields of a file's with these.
pub const RECORD: [u8; RECORD_LEN] = record(
    crate::abi::ABI_VERSION,
    crate::abi::layout(),
    crate::fingerprint::FINGERPRINT,
    crate::sdk::BOUNDARY_HASH,
);

/// The record of a build at `abi_version` with `layout`, the toolchain `fingerprint` and the boundary hash
/// `boundary_hash`: [`MAGIC`], the version and the layout little-endian, the sixteen hex digits, the
/// fingerprint's length (two bytes, little-endian) and its bytes, zero-padded to [`RECORD_LEN`]. A
/// fingerprint longer than [`FINGERPRINT_CAPACITY`], or a boundary hash that is not sixteen bytes, fails the
/// build that bakes it.
pub const fn record(
    abi_version: u32,
    layout: u64,
    fingerprint: &str,
    boundary_hash: &str,
) -> [u8; RECORD_LEN] {
    assert!(boundary_hash.len() == 16, "a boundary hash is sixteen hex digits");
    assert!(
        fingerprint.len() <= FINGERPRINT_CAPACITY,
        "the toolchain fingerprint outgrew the record"
    );
    let mut out = [0u8; RECORD_LEN];
    put(&mut out, 0, &MAGIC);
    put(&mut out, 8, &abi_version.to_le_bytes());
    put(&mut out, 12, &layout.to_le_bytes());
    put(&mut out, 20, boundary_hash.as_bytes());
    put(&mut out, 36, &(fingerprint.len() as u16).to_le_bytes());
    put(&mut out, FIXED_LEN, fingerprint.as_bytes());
    out
}

const fn put(out: &mut [u8; RECORD_LEN], at: usize, bytes: &[u8]) {
    let mut i = 0;
    while i < bytes.len() {
        out[at + i] = bytes[i];
        i += 1;
    }
}

/// A record, decoded.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IdentityRecord {
    /// The interface version the file was built against.
    pub abi_version: u32,
    /// The layout of the rows it reads in place.
    pub layout: u64,
    /// The strategy boundary's hash.
    pub boundary_hash: String,
    /// The toolchain fingerprint.
    pub fingerprint: String,
}

impl IdentityRecord {
    /// The record `bytes` hold, or why they are not one: the wrong size, another magic or format, a
    /// boundary hash that is not lowercase hex, a fingerprint longer than its room or not UTF-8, or a
    /// padding byte that is not zero.
    pub fn decode(bytes: &[u8]) -> Result<IdentityRecord, String> {
        if bytes.len() != RECORD_LEN {
            return Err(format!("its record is {} bytes, not {RECORD_LEN}", bytes.len()));
        }
        if bytes[..8] != MAGIC {
            return Err("its record is not a vike compatibility record of format 1".to_string());
        }
        let abi_version = le(&bytes[8..12]) as u32;
        let layout = le(&bytes[12..20]);
        let boundary = &bytes[20..36];
        if !boundary.iter().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(b)) {
            return Err(
                "its record's boundary hash is not sixteen lowercase hex digits".to_string()
            );
        }
        let length = le(&bytes[36..38]) as usize;
        if length > FINGERPRINT_CAPACITY {
            return Err(format!(
                "its record states a {length}-byte fingerprint in {FINGERPRINT_CAPACITY} bytes"
            ));
        }
        let (fingerprint, padding) = bytes[FIXED_LEN..].split_at(length);
        if padding.iter().any(|&b| b != 0) {
            return Err("its record carries bytes after its fingerprint".to_string());
        }
        let fingerprint = std::str::from_utf8(fingerprint)
            .map_err(|_| "its record's fingerprint is not UTF-8 text".to_string())?;
        Ok(IdentityRecord {
            abi_version,
            layout,
            boundary_hash: String::from_utf8_lossy(boundary).into_owned(),
            fingerprint: fingerprint.to_string(),
        })
    }
}

/// The record of the artifact at `path`, read from its [`IDENTITY_SECTION`] without loading it; why not,
/// in words (the module doc's refusals), otherwise.
pub fn read(path: &Path) -> Result<IdentityRecord, String> {
    let mut file = File::open(path).map_err(|e| format!("it cannot be read ({e})"))?;
    let len = file.metadata().map_err(|e| format!("it cannot be read ({e})"))?.len();
    IdentityRecord::decode(&section(&mut file, len, IDENTITY_SECTION)?)
}

/// The ELF64 header's size, and a section header's.
const HEADER_LEN: u64 = 64;
const SECTION_HEADER_LEN: u64 = 64;
/// `sh_type` of a section that occupies no bytes of the file (`.bss`).
const SHT_NOBITS: u32 = 8;
/// `sh_type` of the reserved null section.
const SHT_NULL: u32 = 0;
/// `e_shstrndx`'s escape to extended numbering, which no artifact needs.
const SHN_XINDEX: u64 = 0xffff;

/// The bytes of the one section named `name` in the 64-bit little-endian ELF file `source` (`len` bytes),
/// read through its section headers alone, every read bounded (the module doc's refusals). `pub` for the
/// parser's own tests, which hand it a built file in memory.
pub fn section<R: Read + Seek>(source: &mut R, len: u64, name: &str) -> Result<Vec<u8>, String> {
    let header = read_at(source, len, 0, HEADER_LEN, "the ELF header")?;
    if header[..4] != *b"\x7fELF" {
        return Err("it is not an ELF file".to_string());
    }
    if header[4] != 2 || header[5] != 1 {
        return Err("it is not a 64-bit little-endian ELF file".to_string());
    }
    let field = |at: usize, n: usize| le(&header[at..at + n]);
    let (header_size, entry_size) = (field(0x34, 2), field(0x3a, 2));
    if header_size != HEADER_LEN || entry_size != SECTION_HEADER_LEN {
        return Err(format!(
            "its ELF header states a {header_size}-byte header and {entry_size}-byte section headers, \
             not {HEADER_LEN} and {SECTION_HEADER_LEN}"
        ));
    }
    let (table_at, count, names_index) = (field(0x28, 8), field(0x3c, 2), field(0x3e, 2));
    if count == 0 || names_index == SHN_XINDEX {
        return Err(
            "its ELF header states no section count (extended numbering is not read)".to_string()
        );
    }
    if count > MAX_SECTIONS {
        return Err(format!("its ELF header states {count} sections, more than {MAX_SECTIONS}"));
    }
    if names_index >= count {
        return Err(format!("its section-name table is section {names_index} of {count}"));
    }
    let table = read_at(source, len, table_at, count * SECTION_HEADER_LEN, "the section headers")?;
    let sections: Vec<SectionHeader> = table
        .as_chunks::<{ SECTION_HEADER_LEN as usize }>()
        .0
        .iter()
        .map(SectionHeader::of)
        .collect();

    // Every section that holds bytes lies inside the file, and none overlaps another or the headers.
    let mut spans = vec![(0, HEADER_LEN, "the ELF header".to_string())];
    spans.push((
        table_at,
        table_at + count * SECTION_HEADER_LEN,
        "the section headers".to_string(),
    ));
    for (index, s) in sections.iter().enumerate() {
        if s.kind == SHT_NOBITS || s.kind == SHT_NULL || s.size == 0 {
            continue;
        }
        let end = s.offset.checked_add(s.size).filter(|end| *end <= len).ok_or_else(|| {
            format!("it is truncated: section {index} ends past the file's {len} bytes")
        })?;
        spans.push((s.offset, end, format!("section {index}")));
    }
    spans.sort_by_key(|(start, end, _)| (*start, *end));
    if let Some(pair) = spans.windows(2).find(|pair| pair[1].0 < pair[0].1) {
        return Err(format!("{} and {} overlap", pair[0].2, pair[1].2));
    }

    let names = &sections[names_index as usize];
    if names.kind == SHT_NOBITS || names.size > MAX_NAME_TABLE {
        return Err(format!(
            "its section-name table is not a table of at most {MAX_NAME_TABLE} bytes"
        ));
    }
    let names = read_at(source, len, names.offset, names.size, "the section-name table")?;
    let mut found = Vec::new();
    for (index, s) in sections.iter().enumerate() {
        let start = usize::try_from(s.name)
            .ok()
            .filter(|at| *at < names.len())
            .ok_or_else(|| format!("section {index}'s name lies outside the section-name table"))?;
        let Some(length) = names[start..].iter().position(|&b| b == 0) else {
            return Err(format!("section {index}'s name runs off the section-name table"));
        };
        if names[start..start + length] == *name.as_bytes() {
            found.push(s);
        }
    }
    let record = match found[..] {
        [one] => one,
        [] => {
            return Err(format!(
                "it has no `{name}` section: it was built before compatibility records (interface \
                 version 15) or not by vike's builder"
            ));
        }
        _ => return Err(format!("it has {} `{name}` sections", found.len())),
    };
    if record.kind == SHT_NOBITS || record.size != RECORD_LEN as u64 {
        return Err(format!(
            "its `{name}` section holds {} bytes in the file, not {RECORD_LEN}",
            if record.kind == SHT_NOBITS { 0 } else { record.size }
        ));
    }
    read_at(source, len, record.offset, record.size, "the record")
}

/// The fields of one section header [`section`] reads.
struct SectionHeader {
    name: u64,
    kind: u32,
    offset: u64,
    size: u64,
}

impl SectionHeader {
    fn of(bytes: &[u8; SECTION_HEADER_LEN as usize]) -> SectionHeader {
        SectionHeader {
            name: le(&bytes[0..4]),
            kind: le(&bytes[4..8]) as u32,
            offset: le(&bytes[24..32]),
            size: le(&bytes[32..40]),
        }
    }
}

/// `bytes` (at most eight) as a little-endian unsigned number.
fn le(bytes: &[u8]) -> u64 {
    bytes.iter().rev().fold(0, |word, &b| (word << 8) | u64::from(b))
}

/// `size` bytes at `offset` of `source` (`len` bytes long), or a refusal naming `what` when they run past
/// its end. Every caller bounds `size` first, so nothing larger than [`MAX_NAME_TABLE`] or
/// [`MAX_SECTIONS`] headers is ever allocated.
fn read_at<R: Read + Seek>(
    source: &mut R,
    len: u64,
    offset: u64,
    size: u64,
    what: &str,
) -> Result<Vec<u8>, String> {
    if offset.checked_add(size).is_none_or(|end| end > len) {
        return Err(format!(
            "it is truncated: {what} ({size} bytes at {offset}) end past the file's {len} bytes"
        ));
    }
    let mut bytes = vec![0; usize::try_from(size).map_err(|_| format!("{what} is too large"))?];
    source
        .seek(SeekFrom::Start(offset))
        .and_then(|_| source.read_exact(&mut bytes))
        .map_err(|e| format!("reading {what}: {e}"))?;
    Ok(bytes)
}

#[path = "identity_tests.rs"]
#[cfg(test)]
mod identity_tests;
