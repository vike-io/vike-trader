//! Test doubles the unit tests and the integration binaries of this crate share (`test-support` only:
//! this crate's own `tests/` reach it through its self-referential dev-dependency, and the
//! datahub's import tests through theirs).

use std::cell::Cell;
use std::io::Write;

use flate2::Compression;
use flate2::write::DeflateEncoder;

/// A one-member zip as Binance writes it: local header, deflate stream, one central entry, end.
pub fn zip_of(name: &str, body: &[u8]) -> Vec<u8> {
    let mut deflate = DeflateEncoder::new(Vec::new(), Compression::default());
    deflate.write_all(body).expect("deflate into memory");
    let stream = deflate.finish().expect("deflate into memory");
    let mut crc = flate2::Crc::new();
    crc.update(body);
    zip_parts(name, 8, &stream, crc.sum(), 1)
}

/// A zip of one local header naming `name` with `method`, then `stream`, then `entries` central
/// entries carrying `crc`, then the end record: the shapes a zip reader must refuse are built with
/// it.
pub fn zip_parts(name: &str, method: u16, stream: &[u8], crc: u32, entries: usize) -> Vec<u8> {
    let name_len = u16::try_from(name.len()).expect("a short name").to_le_bytes();
    let mut out = b"PK\x03\x04\x14\x00\x00\x00".to_vec();
    out.extend(method.to_le_bytes());
    out.extend([0u8; 16]); // time, date, CRC and sizes: a reader here must not rely on them
    out.extend(name_len);
    out.extend([0u8; 2]);
    out.extend(name.as_bytes());
    out.extend(stream);
    for _ in 0..entries {
        out.extend(b"PK\x01\x02");
        out.extend([0u8; 12]);
        out.extend(crc.to_le_bytes());
        out.extend([0u8; 8]);
        out.extend(name_len);
        out.extend([0u8; 16]);
        out.extend(name.as_bytes());
    }
    out.extend(b"PK\x05\x06");
    out.extend([0u8; 18]);
    out
}

/// A `should_stop` probe that answers `false` to its first `k` asks and `true` from then on: "fires
/// after chunk `k`", since an ingest asks once at the top of every chunk. It counts its asks, and it
/// latches the way the datahub's does: once it has said stop, it keeps saying it.
pub struct FiresAfter {
    k: usize,
    /// How many times the ingest asked.
    pub asked: Cell<usize>,
}

impl FiresAfter {
    /// A probe that fires after chunk `k`.
    pub fn new(k: usize) -> Self {
        FiresAfter { k, asked: Cell::new(0) }
    }

    /// One ask: `true` once more than `k` asks were made.
    pub fn ask(&self) -> bool {
        self.asked.set(self.asked.get() + 1);
        self.asked.get() > self.k
    }
}
