//! The client-side profile rewrites (build, preset merge, script inject) and the staged copy.

use std::path::{Path, PathBuf};

use crate::exit::{CliError, CmdResult};

use super::{Origin, Override, PARAMS_WRAPPER_KEY, SRC_KEY};

/// A profile written into `<project>/tmp` for a child process to read, removed when this value is
/// dropped.
///
/// A thin wrapper rather than a bare path because the OWNERSHIP is the point: the directory guard
/// has to outlive the child, and a function returning a `PathBuf` out of a dropped `ScratchDir`
/// would compile and then hand the engine a file that is already gone.
pub(super) struct StagedProfile {
    /// The guard. Never read — its `Drop` is the whole job — and named rather than `_` so it is
    /// obvious that dropping it early is what breaks this.
    _dir: vike_model::scratch::ScratchDir,
    path: PathBuf,
}

impl StagedProfile {
    pub(super) fn write(scratch_root: &Path, profile_toml: &str) -> CmdResult<Self> {
        let dir = vike_model::scratch::ScratchDir::create_in(scratch_root, "vike-cli-local")
            .map_err(|e| {
                CliError::failed(format!(
                    "cannot create a scratch directory under {}: {e}",
                    scratch_root.display()
                ))
            })?;
        let path = dir.path().join("profile.toml");
        std::fs::write(&path, profile_toml).map_err(|e| {
            CliError::failed(format!(
                "cannot stage the rewritten profile at {}: {e}",
                path.display()
            ))
        })?;
        Ok(Self { _dir: dir, path })
    }

    pub(super) fn path(&self) -> &Path {
        &self.path
    }
}

/// Inject an authored Rhai script's `src` into a profile's `[strategy.params].src`, returning the
/// re-serialized profile TOML to ship. Parses the profile as a `toml::Value` (a bad profile is a
/// clean error, not a panic), creates `[strategy]`/`[strategy.params]` if absent, and sets/overwrites
/// `src`. Existing params (the numeric knobs a `[sweep]` varies) are preserved. Any pre-existing
/// inline `src` is overwritten — the `--script` file wins.
pub(crate) fn inject_script_src(profile_toml: &str, script_src: &str) -> Result<String, String> {
    let mut doc: toml::Value =
        toml::from_str(profile_toml).map_err(|e| format!("profile is not valid TOML: {e}"))?;
    strategy_params_mut(&mut doc)?
        .insert(SRC_KEY.to_string(), toml::Value::String(script_src.to_string()));
    toml::to_string(&doc)
        .map_err(|e| format!("cannot re-serialize profile after --script inject: {e}"))
}

/// Merge a PRESET's knobs into a profile's `[strategy.params]`, returning the re-serialized profile
/// TOML to ship — the `--preset` half of the same client-side rewrite [`inject_script_src`] does.
///
/// A preset IS the params table: a FLAT table of a strategy's knobs, whose keys land in
/// `[strategy.params]` one for one, last-wins over anything the profile already set there. Every
/// other key of the profile is untouched.
///
/// # Two shapes are REFUSED, both because accepting them would do nothing visible
///
/// * **A `[params]` wrapper.** Merged as-is it would give the strategy one parameter called
///   `params` that no `from_params` reader looks at, while every knob silently kept its default —
///   the worst available outcome, because the user did everything else right. Silently UNWRAPPING it
///   instead would make two shapes legal and pick between them by an invisible rule.
/// * **A `src` key**, which is the strategy's SOURCE rather than a knob. Allowing it would let a
///   params file smuggle a whole script past `--script`, and two overwrite rules interacting is
///   exactly how a silent surprise gets built.
///
/// ⚠ **This rule is stated in two crates and that is deliberate.**
/// `crates/vike-studio-core/src/user_strategies/load.rs`'s `check_preset_shape` is the authority and
/// carries the full argument; this CLI cannot call it, because `vike-studio-core` depends on
/// `vike-data/hist-datafusion` and this crate's whole identity is being DataFusion-free on the fast
/// lane (see the `[dependencies]` rationale in `crates/vike-cli/Cargo.toml`). The rule is ten lines
/// and the alternative is dragging Arrow into a laptop binary.
pub(crate) fn merge_preset_params(profile_toml: &str, preset_toml: &str) -> Result<String, String> {
    let preset: toml::Value =
        toml::from_str(preset_toml).map_err(|e| format!("not valid TOML: {e}"))?;
    let knobs = preset.as_table().ok_or("a preset must be a table of parameters")?;
    if knobs.len() == 1 && knobs.get(PARAMS_WRAPPER_KEY).is_some_and(toml::Value::is_table) {
        return Err(format!(
            "it wraps its knobs in a [{PARAMS_WRAPPER_KEY}] table, so the strategy would receive \
             one parameter called '{PARAMS_WRAPPER_KEY}' that nothing reads and every knob would \
             keep its default. A preset IS the params table: delete the [{PARAMS_WRAPPER_KEY}] \
             header and leave the keys at the top level"
        ));
    }
    if knobs.contains_key(SRC_KEY) {
        return Err(format!(
            "it defines '{SRC_KEY}', which is the strategy's SOURCE rather than one of its knobs — \
             that is what `--script` is for. Delete the '{SRC_KEY}' key"
        ));
    }

    let mut doc: toml::Value =
        toml::from_str(profile_toml).map_err(|e| format!("profile is not valid TOML: {e}"))?;
    let params = strategy_params_mut(&mut doc)?;
    for (key, value) in knobs {
        params.insert(key.clone(), value.clone());
    }
    toml::to_string(&doc)
        .map_err(|e| format!("cannot re-serialize profile after --preset merge: {e}"))
}

/// The profile's `[strategy.params]` table, CREATING `[strategy]` and `[strategy.params]` when
/// absent — the one place both client-side rewrites above reach into a profile, so they cannot
/// disagree about where params live or about what a non-table there means.
fn strategy_params_mut(
    doc: &mut toml::Value,
) -> Result<&mut toml::map::Map<String, toml::Value>, String> {
    let root = doc.as_table_mut().ok_or("profile root is not a TOML table")?;
    let strategy = root
        .entry("strategy")
        .or_insert_with(|| toml::Value::Table(Default::default()))
        .as_table_mut()
        .ok_or("[strategy] is not a table")?;
    strategy
        .entry("params")
        .or_insert_with(|| toml::Value::Table(Default::default()))
        .as_table_mut()
        .ok_or_else(|| "[strategy.params] is not a table".to_string())
}

/// Type a `--set` VALUE by parsing it as a one-key TOML DOCUMENT (`v = <text>`), falling back to a
/// bare string.
///
/// ⚠ **`toml::Value` has no scalar `FromStr`.** `"2.5".parse::<toml::Value>()` is a DOCUMENT parse
/// in the `toml` crate too, so a bare `2.5` is not a valid document and the obvious implementation
/// types EVERY value as a string. `crates/vike-studio-core/src/spec.rs`'s `parse_scalar` and
/// `crates/vike-studio/src/backend/remote.rs`'s `parse_toml_value` are the two existing implementations of
/// this idiom and both say so in their own comments; neither crate is reachable from this
/// DataFusion-free CLI, so this is a third copy —
/// `crates/vike-cli/tests/backtest_flags_schema.rs` is what holds it to the real profile loader.
///
/// ⚠ `vike_config::write::set_setting` does the same job with `trimmed.parse::<Value>()`, and that
/// is NOT a counter-example: its `Value` is `toml_edit::Value`, which DOES parse a bare scalar.
/// This crate links `toml` and not `toml_edit`, and the two are not interchangeable.
///
/// Wrongly-typed input is not this site's problem to guess at: the profile loader on the far side
/// refuses it with the key's own message.
pub fn parse_scalar(text: &str) -> toml::Value {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return toml::Value::String(String::new());
    }
    match toml::from_str::<toml::Value>(&format!("v = {trimmed}")) {
        Ok(doc) => {
            doc.get("v").cloned().unwrap_or_else(|| toml::Value::String(trimmed.to_string()))
        }
        Err(_) => toml::Value::String(trimmed.to_string()),
    }
}

/// Whether a `--param` VALUE is one of spec §5.4's search-axis spellings rather than a single value.
///
/// Two forms: a bracketed category list (`[trend,chop]`) and a `lo:hi:step` numeric triple. The
/// triple is checked by SHAPE — three colon-separated parts that all parse as `f64` — so an
/// ordinary value that merely CONTAINS a colon (`binance:BTCUSDT`) is untouched.
///
/// ⚠ The residual: a genuine string param spelled `1:2:3` has no `--param` spelling. It is
/// reachable through `--set strategy.params.<k>=1:2:3`, and the refusal names that route.
pub(super) fn looks_like_a_search_axis(raw: &str) -> bool {
    let t = raw.trim();
    if t.starts_with('[') {
        return true;
    }
    let parts: Vec<&str> = t.split(':').collect();
    parts.len() == 3 && parts.iter().all(|p| p.trim().parse::<f64>().is_ok())
}

/// Assign `value` at `dotted_key` inside a parsed profile document, creating the tables the path
/// needs.
///
/// The `vike_config::write::set_setting` walk, transplanted onto `toml::Value`: split on `.`,
/// refuse an empty segment, walk the parents creating tables, refuse a path THROUGH a plain value,
/// refuse a key naming a whole table, then insert. Four refusals, each naming the key.
///
/// ⚠ A key naming a whole TABLE is refused rather than replaced. Overwriting `[engine.impact]` with
/// a scalar is never what an operator meant, and the remedy — set its leaves — is what the message
/// says. The cost is that an inline-table replacement has no `--set` spelling.
///
/// ⚠ It performs NO schema check. `BacktestProfile` is not nameable from this crate (see the
/// manifest's own comment), so an undeclared key is refused on the far side by
/// `#[serde(deny_unknown_fields)]` — serde's message names the valid set, which no local roster
/// could do as well or keep as current. The one check this side DOES make is on the FIRST segment;
/// see [`PROFILE_TOP_LEVEL_KEYS`].
pub fn set_profile_key(
    doc: &mut toml::Value,
    dotted_key: &str,
    value: toml::Value,
) -> Result<(), String> {
    let segs: Vec<&str> = dotted_key.split('.').collect();
    if segs.iter().any(|s| s.trim().is_empty()) {
        return Err(format!(
            "--set key {dotted_key:?} has an empty segment — a key is spelled `table.key` (or \
             `table.sub.key`), with no leading, trailing or doubled dot"
        ));
    }
    let mut cur =
        doc.as_table_mut().ok_or_else(|| "profile root is not a TOML table".to_string())?;
    let (leaf, parents) = segs.split_last().expect("split('.') yields at least one segment");
    for (i, seg) in parents.iter().enumerate() {
        if !cur.contains_key(*seg) {
            cur.insert((*seg).to_string(), toml::Value::Table(Default::default()));
        }
        cur = cur.get_mut(*seg).expect("present or just inserted").as_table_mut().ok_or_else(
            || {
                format!(
                    "`{}` is a plain value in the profile, so `{dotted_key}` cannot nest under it",
                    segs[..=i].join(".")
                )
            },
        )?;
    }
    if cur.get(*leaf).is_some_and(toml::Value::is_table) {
        return Err(format!(
            "`{dotted_key}` names a whole table in the profile, not a single value — set its \
             leaves instead (`{dotted_key}.<key>`)"
        ));
    }
    cur.insert((*leaf).to_string(), value);
    Ok(())
}

/// Build the profile TEXT this invocation ships: the `--profile` file (or an empty document when
/// there is none), with every override applied in spec §5.3's order.
///
/// ```text
/// schema default  →  [profile file]  →  --set  →  named sugar flag
/// ```
///
/// The passes are what make that order STRUCTURAL rather than a convention: a `Sugar` entry and a
/// `Set` entry naming the same key reach the same [`set_profile_key`] call with the same value, so
/// `--fee X` and `--set engine.fee_rate=X` cannot produce different documents (spec §15.2, gated by
/// `sugar_and_set_produce_the_same_document`). [`Origin::Implied`] is a fourth pass the spec's
/// ladder does not name, applied only where nothing else did — it exists because two keys have no
/// schema default and no other flag spelling; see `implied_defaults`.
///
/// ⚠ The output is a NORMALIZED re-serialization. `toml::to_string` over a re-parsed `toml::Value`
/// drops comments and reorders keys, exactly as [`merge_preset_params`] and [`inject_script_src`]
/// already do — so `--write-profile` round-trips the RESOLVED profile, never the operator's file
/// text (spec §15.4).
pub fn build_profile_toml(base: Option<&str>, overrides: &[Override]) -> Result<String, String> {
    let mut doc: toml::Value = match base {
        Some(text) => {
            toml::from_str(text).map_err(|e| format!("profile is not valid TOML: {e}"))?
        }
        None => toml::Value::Table(Default::default()),
    };
    for ov in overrides.iter().filter(|o| matches!(o.origin, Origin::Set)) {
        set_profile_key(&mut doc, &ov.key, ov.value.clone())?;
    }
    for ov in overrides.iter().filter(|o| matches!(o.origin, Origin::Sugar(_))) {
        set_profile_key(&mut doc, &ov.key, ov.value.clone())?;
    }
    for ov in overrides.iter().filter(|o| matches!(o.origin, Origin::Implied(_))) {
        if profile_key_is_set(&doc, &ov.key) {
            continue;
        }
        set_profile_key(&mut doc, &ov.key, ov.value.clone())?;
    }
    toml::to_string(&doc).map_err(|e| format!("cannot re-serialize the built profile: {e}"))
}

/// The `--show-effective` document: the built profile, with every command-line override rendered as
/// a `#` COMMENT HEADER above it.
///
/// ⚠ Comments, not a table, and deliberately: the whole of stdout stays valid TOML, so
/// `vike-cli backtest run --show-effective … > run.toml` produces a usable profile. Two streams would
/// let a redirect silently drop half the artifact.
///
/// ⚠ **Three origins, not spec §5.3's four.** The ladder is
/// `schema default → file → --set → sugar`, and this side CANNOT SEE a schema default: they live in
/// serde attributes inside `vike-backtest`, which this crate does not link (see the manifest's own
/// comment). So what is rendered is what was OBSERVED on this command line — `--set`, a named sugar
/// flag, and this side's own implied defaults — plus a closing note that every other value came
/// from the profile file or from the engine's default. A fourth column would be a value this
/// command cannot compute.
pub fn render_effective(profile_toml: &str, overrides: &[Override]) -> String {
    // ⚠ The BUILT document, re-parsed, because an `Origin::Implied` row is not necessarily in it —
    // see `implied_row_applied`. A document this side just serialized should always re-parse; if it
    // somehow does not, every row renders unannotated rather than the whole command failing.
    let built: Option<toml::Value> = toml::from_str(profile_toml).ok();
    let mut out = String::new();
    out.push_str("# the resolved profile, and where each command-line value came from\n");
    for ov in overrides {
        let origin = match ov.origin {
            Origin::Set => "--set".to_string(),
            Origin::Sugar(flag) => flag.to_string(),
            // ⚠ THE ROW THAT CAN BE A LIE IF IT IS PRINTED BLIND. `build_profile_toml`'s fourth
            // pass SKIPS an implied row whose key something else already set, so a
            // `--profile tick.toml --show-effective` would otherwise print
            // `data.kind "bar" implied` three lines above a document reading `kind = "tick"`.
            // The row is kept rather than dropped because "this is what would have been implied,
            // and it was not used" is more informative than silence.
            Origin::Implied(reason) => match built.as_ref() {
                Some(doc) if !implied_row_applied(doc, overrides, ov) => {
                    format!("implied ({reason}) — NOT APPLIED, the profile already set this key")
                }
                _ => format!("implied ({reason})"),
            },
        };
        out.push_str(&format!("#   {:<32} {:<24} {origin}\n", ov.key, render_one_value(&ov.value)));
    }
    out.push_str(
        "# every other value is the profile file's, or the engine's own schema default — which \
         this side\n# cannot see (it links no engine crate), so it is not listed.\n\n",
    );
    out.push_str(profile_toml);
    out
}

/// The value `dotted_key` resolves to in `doc`, if any — the one walk both the
/// [`Origin::Implied`] guard and [`render_effective`]'s provenance column read.
fn profile_key_lookup<'a>(doc: &'a toml::Value, dotted_key: &str) -> Option<&'a toml::Value> {
    let mut cur = doc;
    for seg in dotted_key.split('.') {
        cur = cur.get(seg)?;
    }
    Some(cur)
}

/// Whether `dotted_key` already resolves to something in `doc` — the [`Origin::Implied`] guard.
fn profile_key_is_set(doc: &toml::Value, dotted_key: &str) -> bool {
    profile_key_lookup(doc, dotted_key).is_some()
}

/// Whether an [`Origin::Implied`] row actually reached the BUILT document, for
/// [`render_effective`]'s provenance column.
///
/// ⚠ **Presence in the final document is NOT the discriminator, and reaching for
/// [`profile_key_is_set`] alone here would be a no-op.** By the time the document is built the key
/// is set either way — that is the whole point of the implied pass — so `profile_key_is_set` on the
/// output is unconditionally `true`. What actually separates the two cases is WHO set it, and there
/// are exactly two ways an implied row loses:
///
/// * another override on this same command line names the same key (`--kind tick`, or
///   `--set data.kind=tick`) — those run in earlier passes and win outright; or
/// * the `--profile` file set it, in which case the built document holds the FILE's value rather
///   than this row's.
///
/// The second test is a value comparison, not a presence one. It has one indistinguishable case,
/// and it is harmless: a file that set the key to the very value this row would have implied reads
/// as "applied". The rendered value is right either way, so nobody is misled about the run.
fn implied_row_applied(doc: &toml::Value, overrides: &[Override], row: &Override) -> bool {
    if overrides.iter().any(|o| o.key == row.key && !matches!(o.origin, Origin::Implied(_))) {
        return false;
    }
    profile_key_lookup(doc, &row.key) == Some(&row.value)
}

/// One override's value as a SINGLE comment-line fragment.
///
/// ⚠ **The one-line property is load-bearing**, not cosmetic. [`render_effective`] puts this inside
/// a `#` comment so that the whole of `--show-effective`'s stdout stays valid TOML and
/// `… --show-effective > run.toml` writes a usable profile. A TABLE value — reachable today through
/// `--set engine.impact={model="sqrt"}`, which nothing refuses ([`set_profile_key`]'s table check
/// fires only when the LEAF is already a table) — serializes as a `[v]` SECTION with its keys on
/// FOLLOWING lines, and a multi-line string value can do the same. Either one would put a raw
/// newline inside the comment and the redirect would write a file that does not parse.
///
/// So only a value that round-trips as a single `v = …` line is rendered; anything else says so and
/// points at the document below, which carries it correctly.
fn render_one_value(value: &toml::Value) -> String {
    let doc = toml::Value::Table([("v".to_string(), value.clone())].into_iter().collect());
    match toml::to_string(&doc) {
        Ok(s) => match s.trim().strip_prefix("v = ") {
            Some(scalar) if !scalar.contains('\n') => scalar.to_string(),
            _ => "<multi-line — see the document below>".to_string(),
        },
        Err(_) => String::from("<unrenderable>"),
    }
}
