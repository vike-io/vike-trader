//! **A zip of several members, read from its central directory**: what the SEC's Form 13F data sets
//! are (`SUBMISSION.tsv`, `COVERPAGE.tsv`, `INFOTABLE.tsv` and others in one zip). Binance's daily zips
//! are ONE member and `crate::venues::binance_archive::decode` reads them by their local header; this
//! reader finds every member through the end record and the central directory, so a member written with
//! a data descriptor (sizes after the data) is read the same way.
//!
//! Stored and deflated members only; a ZIP64 archive (a member or an offset past 4 GiB) is refused,
//! never misread. A member is read as a stream ([`member_reader`]), so a large table is scanned without
//! being inflated whole into memory.

use std::io::Read;

use flate2::read::DeflateDecoder;

/// The end-of-central-directory record's signature, `PK\x05\x06`.
const END_SIGNATURE: [u8; 4] = *b"PK\x05\x06";
/// A central-directory entry's signature, `PK\x01\x02`.
const CENTRAL_SIGNATURE: [u8; 4] = *b"PK\x01\x02";
/// A local file header's signature, `PK\x03\x04`.
const LOCAL_SIGNATURE: [u8; 4] = *b"PK\x03\x04";
/// The end record's fixed size; its comment follows, at most `u16::MAX` bytes.
const END_RECORD_BYTES: usize = 22;
/// A central entry's fixed size; its name, extra field and comment follow.
const CENTRAL_ENTRY_BYTES: usize = 46;
/// A local header's fixed size; its name and extra field follow.
const LOCAL_HEADER_BYTES: usize = 30;
/// The value a 32-bit size or offset holds when the real one is in a ZIP64 extra field.
const ZIP64_MARKER: u32 = u32::MAX;

/// One member, as the central directory lists it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ZipMember {
    /// Its name, as stored.
    pub name: String,
    method: u16,
    compressed: u64,
    local_offset: u64,
}

/// Every member of `zip`, in directory order.
pub fn zip_members(zip: &[u8]) -> Result<Vec<ZipMember>, String> {
    if zip.len() < END_RECORD_BYTES {
        return Err("it is not a zip: shorter than an end record".to_string());
    }
    let floor = zip.len().saturating_sub(END_RECORD_BYTES + usize::from(u16::MAX));
    let end = (floor..=zip.len().saturating_sub(END_RECORD_BYTES))
        .rev()
        .find(|&at| zip[at..at + 4] == END_SIGNATURE)
        .ok_or("it is not a zip: no end-of-central-directory record")?;
    let entries = usize::from(u16_at(zip, end + 10)?);
    let mut at = usize::try_from(u32_at(zip, end + 16)?).map_err(|_| "offset")?;
    let mut out = Vec::with_capacity(entries);
    for _ in 0..entries {
        if zip.get(at..at + 4) != Some(&CENTRAL_SIGNATURE[..]) {
            return Err("its central directory is not where its end record says".to_string());
        }
        let method = u16_at(zip, at + 10)?;
        let compressed = u32_at(zip, at + 20)?;
        let offset = u32_at(zip, at + 42)?;
        if compressed == ZIP64_MARKER || offset == ZIP64_MARKER {
            return Err("it is a ZIP64 archive (a member or an offset past 4 GiB), not read here"
                .to_string());
        }
        let name_len = usize::from(u16_at(zip, at + 28)?);
        let skip =
            name_len + usize::from(u16_at(zip, at + 30)?) + usize::from(u16_at(zip, at + 32)?);
        let name = zip
            .get(at + CENTRAL_ENTRY_BYTES..at + CENTRAL_ENTRY_BYTES + name_len)
            .ok_or("a central entry is cut short")?;
        out.push(ZipMember {
            name: String::from_utf8_lossy(name).into_owned(),
            method,
            compressed: u64::from(compressed),
            local_offset: u64::from(offset),
        });
        at += CENTRAL_ENTRY_BYTES + skip;
    }
    Ok(out)
}

/// The member named `name` (exactly, or as the last segment of a stored path), or the refusal.
pub fn find_member<'m>(members: &'m [ZipMember], name: &str) -> Result<&'m ZipMember, String> {
    members
        .iter()
        .find(|m| m.name == name || m.name.rsplit('/').next() == Some(name))
        .ok_or_else(|| format!("it holds no {name}"))
}

/// A stream of `member`'s uncompressed bytes.
pub fn member_reader<'z>(zip: &'z [u8], member: &ZipMember) -> Result<Box<dyn Read + 'z>, String> {
    let at = usize::try_from(member.local_offset).map_err(|_| "offset")?;
    if zip.get(at..at + 4) != Some(&LOCAL_SIGNATURE[..]) {
        return Err(format!("{}: no local header where the directory says", member.name));
    }
    let start = at
        + LOCAL_HEADER_BYTES
        + usize::from(u16_at(zip, at + 26)?)
        + usize::from(u16_at(zip, at + 28)?);
    let length = usize::try_from(member.compressed).map_err(|_| "size")?;
    let data = zip
        .get(start..start + length)
        .ok_or_else(|| format!("{}: the member's data is cut short", member.name))?;
    match member.method {
        0 => Ok(Box::new(data)),
        8 => Ok(Box::new(DeflateDecoder::new(data))),
        other => {
            Err(format!("{}: compression method {other} is not stored or deflate", member.name))
        }
    }
}

fn u16_at(zip: &[u8], at: usize) -> Result<u16, String> {
    zip.get(at..at + 2)
        .map(|b| u16::from_le_bytes([b[0], b[1]]))
        .ok_or_else(|| "a zip record is cut short".to_string())
}

fn u32_at(zip: &[u8], at: usize) -> Result<u32, String> {
    zip.get(at..at + 4)
        .map(|b| u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
        .ok_or_else(|| "a zip record is cut short".to_string())
}
