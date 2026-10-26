//! Profile selection (the ruling): which of the profile names it was handed wins.

// ---------------------------------------------------------------------------------------------
// THE RULING: selection
// ---------------------------------------------------------------------------------------------

/// Where a resolved profile selection CAME FROM.
///
/// It is reported rather than merely used, and that is §7 of the schema spec (`shadowed` — *"a file
/// could never report this, because a file cannot know it lost"*). With the row winning, an operator
/// whose `ExecStart --config` has stopped deciding anything must be told so at startup, or the
/// ruling delivers the exact defect it was ruling against: positive confirmation of something
/// false.
///
/// ⚠ There was a third rung, an environment variable (`VIKE_RUN_PROFILE`, the run profile's), and
/// it is gone with that variable: decision 0111 made the run profile rows only, and the variable is
/// refused at startup (`vike_config::REMOVED_ENV`). No selector reads the environment.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SelectionSource {
    /// A `profile` row with `active = 1`. **The winner, by the owner's ruling.**
    StoreRow,
    /// An explicit path the caller was given — the daemon's `--config <path>`, RETIRED by 0086 and
    /// still accepted (and ignored) by `crates/vike-tradehub/src/tradehub_cli/args.rs`'s
    /// `Args`.
    ExplicitArg,
}

impl SelectionSource {
    /// The word a startup line prints.
    #[must_use]
    pub fn word(self) -> &'static str {
        match self {
            SelectionSource::StoreRow => "store row",
            SelectionSource::ExplicitArg => "explicit argument",
        }
    }
}

/// One rung that held a value and LOST.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Shadowed {
    /// Which rung.
    pub source: SelectionSource,
    /// What it held. A profile NAME or a path — never a secret.
    pub value: String,
}

/// The outcome of [`select`]: who won, with what, and who lost while still holding a value.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Selected {
    /// The winning value, or `None` when no rung held one.
    pub value: Option<String>,
    /// Which rung won. `None` exactly when [`Self::value`] is `None`.
    pub source: Option<SelectionSource>,
    /// Every rung that held a value and did not win, highest first.
    pub shadowed: Vec<Shadowed>,
}

impl Selected {
    /// Did a store row beat something that is still set? The one condition a startup line must
    /// WARN about rather than merely record.
    #[must_use]
    pub fn row_shadowed_something(&self) -> bool {
        self.source == Some(SelectionSource::StoreRow) && !self.shadowed.is_empty()
    }
}

/// **THE OWNER'S RULING, WRITTEN ONCE: the row wins.**
///
/// Precedence, highest first: an active `profile` row, then an explicit argument. Every rung that
/// held a value and lost is reported in [`Selected::shadowed`], so a caller can say what stopped
/// mattering.
///
/// A blank or whitespace-only value is treated as ABSENT at every rung — the same reading
/// `vike_paths::state_path`'s `VIKE_SETTINGS_DIR` handling gives an empty override, and the reason
/// is the same: `--config=` produced by an unset shell variable in a unit is somebody unsetting it,
/// not somebody naming a profile called "".
#[must_use]
pub fn select(row: Option<&str>, explicit: Option<&str>) -> Selected {
    let rungs: [(SelectionSource, Option<&str>); 2] =
        [(SelectionSource::StoreRow, row), (SelectionSource::ExplicitArg, explicit)];
    let mut out = Selected::default();
    for (source, held) in rungs {
        let Some(v) = held.map(str::trim).filter(|v| !v.is_empty()) else { continue };
        if out.value.is_none() {
            out.value = Some(v.to_string());
            out.source = Some(source);
        } else {
            out.shadowed.push(Shadowed { source, value: v.to_string() });
        }
    }
    out
}
