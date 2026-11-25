# SDK adapters

`adapters/sdk-N/` lets a strategy file pinned to SDK N, cut with another strategy boundary, build against this
host's boundary (decision 0296's step 6): its marker `ADAPTER` (the SDK and the boundary it adapts from) and its
`face.rs`, which stands for `src/guest/face.rs`. `build.rs` compiles every adapter here into this crate
(`crates/vike-rust-strategy/src/sdk.rs`'s `built_in_adapters`), so a vike serves the adapters it was built with.

This file keeps the folder in every tree, held or not: `build.rs` watches the folder unconditionally, so an
adapter added later always re-runs it, and the builder's glue copies the folder with the rest of the boundary
crate (`crates/vike-strategy-builder/src/render.rs`'s `GLUE_PARTS`).
