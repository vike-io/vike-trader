//! Regenerate vike-ctrader's committed protobuf bindings — the drift gate's regen step.
//!
//! Mirrors the exact codegen the (now removed) build.rs performed: default `prost_build::Config`
//! over the same three vendored proto entry points and the same include root, with `PROTOC`
//! pointed at the vendored binary. The ONLY difference is the output destination — a real build
//! script writes to `$OUT_DIR`; this binary writes to a caller-chosen dir (argv[1], default
//! `./out`) via `Config::out_dir`, which changes WHERE the file lands but not its BYTES.
//!
//! cTrader's proto uses no `package`, so prost emits a single `_.rs`. CI compares that emitted
//! `_.rs` against the committed `../src/generated/openapi.rs` byte-for-byte.
//!
//! Run (from anywhere): `cargo run --locked --manifest-path <this>/Cargo.toml -- <out_dir>`.

use std::path::PathBuf;

fn main() {
    // Resolve paths from this crate's manifest dir, not CWD, so the tool works from any directory.
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let proto_root = manifest_dir.join("..").join("proto");

    let out_dir =
        std::env::args().nth(1).map(PathBuf::from).unwrap_or_else(|| manifest_dir.join("out"));
    std::fs::create_dir_all(&out_dir).expect("create out dir");

    // Same as the old build.rs: no system protoc is assumed; use the vendored binary.
    std::env::set_var("PROTOC", protoc_bin_vendored::protoc_bin_path().unwrap());

    let files = [
        proto_root.join("OpenApiCommonMessages.proto"),
        proto_root.join("OpenApiModelMessages.proto"),
        proto_root.join("OpenApiMessages.proto"),
    ];

    // `Config::new()` == what `compile_protos` uses; `.out_dir` only redirects the write target.
    prost_build::Config::new()
        .out_dir(&out_dir)
        .compile_protos(&files, &[proto_root])
        .expect("cTrader proto codegen");

    println!("regenerated -> {}", out_dir.join("_.rs").display());
}
