//! The one error type this crate returns.
//!
//! Hand-written `Display`, no `thiserror`: the whole point of the crate is that a live binary can
//! link its inference half without pulling anything behind it, and twenty lines of `match` cost
//! less than a derive-macro dependency in that binary's audit surface.

use std::fmt;

/// Everything `vike-ml` can refuse to do.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum MlError {
    /// The model text is not the format this parser understands. `line` is 1-based.
    Parse { line: usize, what: String },
    /// A `version=` header this parser refuses outright rather than best-effort parsing.
    ///
    /// Deliberately its own variant: LightGBM's text format is NOT guaranteed stable across major
    /// versions (models saved by v3 fail to load in v4), so a version mismatch is a hard error and
    /// never a warning — a best-effort parse of an unknown layout produces wrong numbers silently.
    ///
    /// ⚠ Its message is deliberately long, because a refusal an operator cannot ACT on is a
    /// refusal the next person widens. See [`crate::pins`] for what it names and why.
    UnsupportedVersion(String),
    /// A model the walker has no implementation for (a non-binary objective, multiclass, a linear
    /// tree). The message names what was seen.
    Unsupported(String),
    /// Row / feature counts that cannot describe the model or the data.
    Shape(String),
    /// Reading or writing a file the CALLER named failed.
    Io(String),
    /// The LightGBM binary could not be run, or is not the version this crate is pinned to.
    ///
    /// Deliberately distinct from [`MlError::Backend`]: this one says *the thing I was told to
    /// drive is not the thing I expect*, which is an OPERATOR fault with an operator fix, while
    /// `Backend` says *LightGBM ran and refused*, which is a data or parameter fault.
    Trainer(String),
    /// LightGBM itself ran and refused — a non-zero exit, with its own stderr attached.
    Backend(String),
}

impl fmt::Display for MlError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Parse { line, what } => write!(f, "model text, line {line}: {what}"),
            Self::UnsupportedVersion(v) => write!(
                f,
                "model text declares `version={v}`; this walker implements `{}` only, and is \
                 pinned to upstream LightGBM `{}`. LightGBM's text format is not stable across \
                 major versions — a best-effort parse of an unknown layout returns plausible \
                 WRONG numbers rather than failing. To bump: change `crate::pins`, rebuild the \
                 binary (`just lightgbm-build`), re-run `tests/train_infer_equality.rs` on the \
                 box that holds it — THAT is the proof, not this check — and regenerate the golden \
                 fixtures with \
                 `capture_the_golden_fixture`. `crates/vike-ml/CLAUDE.md` is the procedure.",
                crate::pins::MODEL_VERSION,
                crate::pins::PINNED_LIGHTGBM_TAG,
            ),
            Self::Unsupported(what) => write!(f, "unsupported model: {what}"),
            Self::Shape(what) => write!(f, "shape: {what}"),
            Self::Io(what) => write!(f, "file: {what}"),
            // ⚠ No box name in this text (it used to say "the CI box only"): `Display` strings are bytes
            // in every binary that links this crate, `vike-app` among them, and the release's
            // box-path guard (`scripts/refuse_box_paths.sh`) refuses a public asset that names a
            // box. The fact survives without the name — the recipe builds on ONE box, the one
            // whose toolchain compiles upstream C++.
            Self::Trainer(what) => write!(
                f,
                "the LightGBM trainer is not usable: {what}. Build the pinned upstream binary with \
                 `just lightgbm-build` (on the training box only — it clones and compiles upstream \
                 C++), or pass the path to one that already exists. Inference needs no binary at \
                 all and is always available."
            ),
            Self::Backend(what) => write!(f, "LightGBM refused: {what}"),
        }
    }
}

impl std::error::Error for MlError {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_missing_trainer_binary_names_the_path_and_the_pin() {
        let msg =
            MlError::Trainer("/no/such/lightgbm: No such file or directory".into()).to_string();
        assert!(msg.contains("/no/such/lightgbm"), "{msg}");
        assert!(
            msg.contains("just lightgbm-build"),
            "the message must name the recipe that \
                                                      produces the binary: {msg}"
        );
        assert!(
            msg.contains("Inference needs no"),
            "...and must say the OTHER half still \
                                                     works: {msg}"
        );
    }

    #[test]
    fn a_parse_error_names_the_line_it_died_on() {
        let msg = MlError::Parse { line: 42, what: "not a `key=value` line".into() }.to_string();
        assert!(msg.contains("42"), "{msg}");
        assert!(msg.contains("not a `key=value` line"), "{msg}");
    }

    #[test]
    fn a_refused_version_repeats_the_version_it_saw() {
        let msg = MlError::UnsupportedVersion("v3".into()).to_string();
        assert!(msg.contains("v3"), "{msg}");
        assert!(msg.contains("v4"), "the message must name what IS supported: {msg}");
    }

    #[test]
    fn it_is_a_std_error() {
        fn assert_error<E: std::error::Error>(_: &E) {}
        assert_error(&MlError::Shape("x".into()));
    }
}
