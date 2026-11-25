use std::io::Cursor;

use super::*;

/// One section of a miniature ELF file: its name, its `sh_type` and its bytes.
struct Part {
    name: String,
    kind: u32,
    data: Vec<u8>,
}

fn part(name: &str, kind: u32, data: &[u8]) -> Part {
    Part { name: name.to_string(), kind, data: data.to_vec() }
}

/// A well-formed 64-bit little-endian ELF file holding `parts` after the header, then its name table,
/// then the section headers (the null section first, the name table last): the shape a linker writes.
fn elf(parts: &[Part]) -> Vec<u8> {
    let mut names = vec![0u8];
    let mut name_at = Vec::new();
    for name in parts.iter().map(|p| p.name.as_str()).chain([".shstrtab"]) {
        name_at.push(names.len() as u64);
        names.extend_from_slice(name.as_bytes());
        names.push(0);
    }
    let mut file = vec![0u8; 64];
    let mut headers = vec![[0u8; 64]];
    for (index, p) in parts.iter().enumerate() {
        let offset = file.len() as u64;
        if p.kind != SHT_NOBITS {
            file.extend_from_slice(&p.data);
        }
        headers.push(section_header(name_at[index], p.kind, offset, p.data.len() as u64));
    }
    let names_at = file.len() as u64;
    file.extend_from_slice(&names);
    headers.push(section_header(name_at[parts.len()], 3, names_at, names.len() as u64));
    let table_at = file.len() as u64;
    for h in &headers {
        file.extend_from_slice(h);
    }
    file[..4].copy_from_slice(b"\x7fELF");
    file[4..7].copy_from_slice(&[2, 1, 1]);
    file[0x28..0x30].copy_from_slice(&table_at.to_le_bytes());
    file[0x34..0x36].copy_from_slice(&64u16.to_le_bytes());
    file[0x3a..0x3c].copy_from_slice(&64u16.to_le_bytes());
    file[0x3c..0x3e].copy_from_slice(&(headers.len() as u16).to_le_bytes());
    file[0x3e..0x40].copy_from_slice(&(headers.len() as u16 - 1).to_le_bytes());
    file
}

fn section_header(name: u64, kind: u32, offset: u64, size: u64) -> [u8; 64] {
    let mut h = [0u8; 64];
    h[0..4].copy_from_slice(&(name as u32).to_le_bytes());
    h[4..8].copy_from_slice(&kind.to_le_bytes());
    h[24..32].copy_from_slice(&offset.to_le_bytes());
    h[32..40].copy_from_slice(&size.to_le_bytes());
    h
}

/// Where section `index`'s header starts in `file`.
fn header_at(file: &[u8], index: usize) -> usize {
    le(&file[0x28..0x30]) as usize + index * 64
}

/// Section `index`'s `sh_offset` set to `offset`, or its `sh_size` to `size`.
fn set_offset(file: &mut [u8], index: usize, offset: u64) {
    let at = header_at(file, index) + 24;
    file[at..at + 8].copy_from_slice(&offset.to_le_bytes());
}

fn set_size(file: &mut [u8], index: usize, size: u64) {
    let at = header_at(file, index) + 32;
    file[at..at + 8].copy_from_slice(&size.to_le_bytes());
}

fn found(file: &[u8]) -> Result<Vec<u8>, String> {
    section(&mut Cursor::new(file), file.len() as u64, IDENTITY_SECTION)
}

fn refused(file: &[u8], words: &str) {
    match found(file) {
        Ok(bytes) => panic!("expected a refusal naming {words:?}, read {} bytes", bytes.len()),
        Err(why) => assert!(why.contains(words), "{why}\nwanted: {words}"),
    }
}

/// A file as the template builds it: code, the record, some data.
fn artifact() -> Vec<u8> {
    elf(&[
        part(".text", 1, &[0x90; 48]),
        part(IDENTITY_SECTION, 1, &RECORD),
        part(".data", 1, &[7; 16]),
        part(".bss", SHT_NOBITS, &[0; 32]),
    ])
}

#[test]
fn this_builds_record_decodes_to_its_own_values() {
    let record = IdentityRecord::decode(&RECORD).expect("this build's record");
    assert_eq!(
        record,
        IdentityRecord {
            abi_version: crate::abi::ABI_VERSION,
            layout: crate::abi::layout(),
            boundary_hash: crate::sdk::BOUNDARY_HASH.to_string(),
            fingerprint: crate::fingerprint::FINGERPRINT.to_string(),
        }
    );
    let other = record_of(13, 7, "rustc=x", "0123456789abcdef");
    assert_eq!(
        IdentityRecord::decode(&other).expect("another build's"),
        IdentityRecord {
            abi_version: 13,
            layout: 7,
            boundary_hash: "0123456789abcdef".to_string(),
            fingerprint: "rustc=x".to_string(),
        }
    );
}

fn record_of(abi: u32, layout: u64, fingerprint: &str, boundary: &str) -> [u8; RECORD_LEN] {
    record(abi, layout, fingerprint, boundary)
}

#[test]
fn a_built_file_yields_its_record() {
    assert_eq!(found(&artifact()).expect("the record"), RECORD);
}

#[test]
fn a_missing_or_doubled_record_section_is_refused() {
    let none = elf(&[part(".text", 1, &[0x90; 48])]);
    refused(&none, "has no `.vike_identity` section");
    let two = elf(&[part(IDENTITY_SECTION, 1, &RECORD), part(IDENTITY_SECTION, 1, &RECORD)]);
    refused(&two, "has 2 `.vike_identity` sections");
}

#[test]
fn a_record_section_of_another_size_or_with_no_bytes_is_refused() {
    refused(
        &elf(&[part(IDENTITY_SECTION, 1, &RECORD[..100])]),
        "holds 100 bytes in the file, not 256",
    );
    refused(&elf(&[part(IDENTITY_SECTION, SHT_NOBITS, &RECORD)]), "holds 0 bytes in the file");
}

#[test]
fn a_file_that_is_not_64_bit_little_endian_elf_is_refused() {
    let mut text = b"this is not an ELF file".to_vec();
    text.resize(64, b' ');
    refused(&text, "it is not an ELF file");
    for (at, value) in [(4, 1), (5, 2)] {
        let mut file = artifact();
        file[at] = value;
        refused(&file, "not a 64-bit little-endian ELF file");
    }
}

#[test]
fn a_truncated_file_is_refused() {
    let file = artifact();
    refused(&file[..40], "truncated: the ELF header");
    refused(&file[..file.len() - 1], "truncated: the section headers");
    let mut past = artifact();
    set_offset(&mut past, 2, file.len() as u64 - 10);
    refused(&past, "truncated: section 2 ends past");
    let mut wraps = artifact();
    set_offset(&mut wraps, 1, u64::MAX - 4);
    refused(&wraps, "truncated: section 1");
    let mut table = artifact();
    table[0x28..0x30].copy_from_slice(&u64::MAX.to_le_bytes());
    refused(&table, "truncated: the section headers");
}

#[test]
fn an_oversized_header_is_refused() {
    let mut many = artifact();
    many[0x3c..0x3e].copy_from_slice(&5000u16.to_le_bytes());
    refused(&many, "states 5000 sections, more than 4096");
    let mut header = artifact();
    header[0x34..0x36].copy_from_slice(&128u16.to_le_bytes());
    refused(&header, "states a 128-byte header");
    let mut entries = artifact();
    entries[0x3a..0x3c].copy_from_slice(&80u16.to_le_bytes());
    refused(&entries, "80-byte section headers");
    let mut extended = artifact();
    extended[0x3c..0x3e].copy_from_slice(&0u16.to_le_bytes());
    refused(&extended, "no section count");
    let mut names = artifact();
    names[0x3e..0x40].copy_from_slice(&9u16.to_le_bytes());
    refused(&names, "section-name table is section 9 of 6");
    let long_name = "x".repeat(70_000);
    let huge_names = elf(&[part(&long_name, 1, &[1]), part(IDENTITY_SECTION, 1, &RECORD)]);
    refused(&huge_names, "not a table of at most 65536 bytes");
}

#[test]
fn overlapping_sections_are_refused() {
    let file = artifact();
    let mut two = file.clone();
    set_offset(&mut two, 3, le(&file[header_at(&file, 2) + 24..header_at(&file, 2) + 32]) + 8);
    refused(&two, "section 2 and section 3 overlap");
    let mut into_header = file.clone();
    set_offset(&mut into_header, 1, 32);
    refused(&into_header, "the ELF header and section 1 overlap");
    let mut grown = file.clone();
    set_size(&mut grown, 1, 48 + 8);
    refused(&grown, "section 1 and section 2 overlap");
    let table_at = le(&file[0x28..0x30]);
    let mut into_table = file;
    set_offset(&mut into_table, 3, table_at);
    refused(&into_table, "overlap");
}

#[test]
fn a_name_outside_the_name_table_is_refused() {
    let mut file = artifact();
    let at = header_at(&file, 1);
    file[at..at + 4].copy_from_slice(&u32::MAX.to_le_bytes());
    refused(&file, "section 1's name lies outside the section-name table");
}

#[test]
fn a_malformed_record_is_refused() {
    let words = |bytes: &[u8]| IdentityRecord::decode(bytes).expect_err("a malformed record");
    assert!(words(&RECORD[..RECORD_LEN - 1]).contains("255 bytes, not 256"));
    let mut magic = RECORD;
    magic[7] = 2;
    assert!(words(&magic).contains("not a vike compatibility record of format 1"));
    let mut boundary = RECORD;
    boundary[20] = b'G';
    assert!(words(&boundary).contains("boundary hash is not sixteen lowercase hex digits"));
    let mut length = RECORD;
    length[36..38].copy_from_slice(&219u16.to_le_bytes());
    assert!(words(&length).contains("219-byte fingerprint in 218 bytes"));
    let mut padding = RECORD;
    padding[RECORD_LEN - 1] = 1;
    assert!(words(&padding).contains("bytes after its fingerprint"));
    let mut text = record_of(15, 1, "ab", "0123456789abcdef");
    text[FIXED_LEN] = 0xff;
    assert!(words(&text).contains("not UTF-8"));
}

/// The templates spell the section by name in their attribute; it is this module's.
#[test]
fn both_templates_bake_the_record_into_this_section() {
    for template in [crate::TEMPLATE_LIB_RS, crate::TEMPLATE_STUDY_LIB_RS] {
        assert!(template.contains(&format!("link_section = \"{IDENTITY_SECTION}\"")), "{template}");
        assert!(template.contains("::vike_rust_strategy::identity::RECORD;"));
        assert!(template.contains("pub extern \"C\" fn vike_strategy_identity() -> *const u8"));
    }
}
