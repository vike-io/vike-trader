// Whether THIS build may freeze its identity: the release guard on the CI freeze switch.
//
// ⚠ **`//` and not the house `//!`, for the reason `src/timefmt.rs` gives:** `build.rs` `include!`s
// this file, and an inner doc comment is only legal before the first item of the including file.
//
// ⚠ **Compiled twice, like `src/timefmt.rs`, and for the same reason:** the build script is the
// only caller that matters, and a build script's own code is never compiled as a test. So
// `crates/vike-buildinfo/src/lib.rs` compiles this file as a `#[cfg(test)]` module and its tests
// drive the decision over the inputs CI never produces. The END-TO-END half is a different test:
// `.github/workflows/ci.yml`'s `plan` job builds `--release` WITH the switch set, and
// `crates/vike-buildinfo/src/lib.rs`'s `a_release_build_is_never_frozen` reads the result.
//
// ⚠ **The variable's NAME is deliberately not in this file.** It is a `src/` file, and the settings
// registry harvests every `VIKE_`-shaped string literal under `src/` as a library read
// (`crates/vike-buildinfo/build.rs`'s module doc tells that story for the generated constants). The
// build script reads the variable and hands the value in.

/// Freeze only on the exact switch value `1`, and only when cargo reports the profile `debug`.
///
/// It FAILS CLOSED in both arguments. A switch spelled `true`, `yes`, ` 1` or `0` does not freeze:
/// one spelling, so an accidental value is a real build rather than a frozen one. And the profile
/// is matched POSITIVELY on `debug` rather than negatively on `release`: cargo reports `release`
/// for the release profile and for every profile that inherits from it, and `debug` for everything
/// else, so the two tests agree today — but a value cargo has never reported (or no value at all)
/// takes the real git probe instead of a frozen one, which is the direction that cannot ship a
/// binary with no commit in it.
pub(crate) fn freeze_applies(switch: Option<&str>, profile: Option<&str>) -> bool {
    switch == Some("1") && profile == Some("debug")
}
