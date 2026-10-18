//! The ONE spelling of the KEY-LESS datahub handshake, server side, that this crate's scripted fake
//! datahubs open every connection with. Each user declares it with `mod common;` (a directory test
//! with `#[path = "../common/mod.rs"]`); a `tests/` SUBDIRECTORY module is not a test target, so this
//! file builds no binary and runs no test of its own. Every binary that declares it calls
//! everything in it, which is why it carries no `allow(dead_code)`.
//!
//! Only the handshake is shared: what each fake answers AFTER it (`Backfill`, `VenueCatalog`,
//! `MdSubscribe`/`MdUpdate`) is that suite's own script and stays in its file.

use std::net::TcpStream;

use vike_datahub_client::PROTO_VERSION;
use vike_datahub_client::proto::{Request, Response, read_frame, write_frame};

/// Read the client's `Request::Hello`, then answer a `Response::Welcome` advertising `features`
/// with NO nonce: the byte-identical-to-pre-auth shape `Response::Welcome::nonce`'s doc describes.
/// `false` when the first frame is not a `Hello` or the `Welcome` cannot be written; the caller
/// decides whether that is a panic or a quiet hang-up.
pub fn accept_keyless(stream: &mut TcpStream, features: Vec<String>) -> bool {
    if !matches!(read_frame::<_, Request>(stream), Ok(Request::Hello { .. })) {
        return false;
    }
    write_frame(stream, &Response::Welcome { proto_version: PROTO_VERSION, features, nonce: None })
        .is_ok()
}
