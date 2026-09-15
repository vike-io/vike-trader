//! The whole of what a Rhai study may CALL: one `rhai::Engine`, and every host function
//! registered onto it.
//!
//! Separated from `mod.rs` for the same reason `crates/vike-script/src/engine.rs` is separated
//! from its `strategy.rs`: the runner is about the LIFECYCLE (compile once, call `run`, recover a
//! typed fault), and this file is about the SURFACE. They change for different reasons, and the
//! surface is the half a reviewer has to read in full — because in this tier the registration list
//! IS the boundary, not a summary of one.
//!
//! # The three families
//!
//! * **`ctx.*`** — [`vike_user_research::StudyContext`]'s own read verbs, one Rhai method per Rust
//!   method, plus the fitting surface argued in [`super`]'s module doc. Nothing here reaches the
//!   store except through the handle the CALLER built.
//! * **the row types** — `Bar` / `QuoteTick` / `TradeTick` / `BookUpdate` registered as custom
//!   types with property getters, so a small scan reads naturally (`b.close`), and [`column`],
//!   which is how a real one is read (see its doc for why a per-row loop is not the answer).
//! * **`StudyOutcome`** — the return value's two mutators, wrapped so a Rust `StudyError` becomes a
//!   Rhai runtime error instead of a value the script can ignore.
//!
//! # ⚠ Two names this file deliberately does NOT use
//!
//! `rhai::Engine::register_fn` REPLACES a previous registration of the same name and arity rather
//! than erroring — the hazard `crates/vike-script/src/engine.rs`'s `HOST_FN_NAMES` exists for. Two
//! bite here:
//!
//! * **`range`** is rhai's OWN two-argument iterator constructor (its `iter_basic` package), and
//!   `for i in range(0, n)` is the idiomatic spelling of a counted loop. Registering a
//!   [`TsRange`] constructor under that name would silently take it away from every study in the
//!   workspace, so the constructor is `ts_range` / `ts_range_all`.
//! * **`print` / `debug`** are left registered and REDIRECTED rather than removed
//!   ([`build_engine`]), because a study's diagnostics are worth keeping — but never on stdout: a
//!   root whose stdout is a protocol (`vike-cli mcp`) would be corrupted by a script's `print`.
//!
//! Every other name here is either a METHOD (dispatched on its receiver's type, so it cannot
//! shadow a free function) or one of `column` / `outcome` / `ts_range` / `ts_range_all`, none of
//! which rhai defines.

use std::sync::Arc;

use rhai::{Dynamic, EvalAltResult};
use vike_data::TsRange;
use vike_ml::{Capture, DEFAULT_POINT, FitImportance, GridPoint, ProbaModel, TrainData};
use vike_model::{Bar, BookUpdate, Level, QuoteTick, TradeTick};
use vike_user_research::{StudyContext, StudyError, StudyOutcome};

use super::fault::{FaultLog, raise};

/// The operation ceiling one `run` call may spend.
///
/// ⚠ It is ~500x `crates/vike-script/src/engine.rs`'s strategy ceiling, and the difference is the
/// UNIT OF WORK rather than a relaxation of standards: a strategy hook is called once per bar and
/// must finish inside a bar, so 2M operations there is generous; a study is called ONCE for a
/// whole window, so the same number would refuse a plain loop over a hundred thousand bars — the
/// most ordinary thing a study does.
///
/// The size limits (`max_array_size` / `max_string_size` / `max_map_size`) are deliberately left
/// UNSET, which in rhai means unlimited. They would bound the wrong thing here: the big arrays a
/// study holds are the ones the HOST handed it out of the store, and a cap low enough to matter
/// would refuse a legitimate window while a script that allocates in a loop is already bounded by
/// this counter.
pub const MAX_OPERATIONS: u64 = 1_000_000_000;

/// The recursion ceiling — the same value the strategy tier uses, because the hazard is the same
/// (a runaway recursion is a stack overflow, which no operation counter catches in time).
pub const MAX_CALL_LEVELS: usize = 64;

/// A model fitted by a study, as a Rhai value.
///
/// `Arc<dyn ProbaModel>` rather than the `Box` [`vike_user_research::CapturedStudyFit`] hands back:
/// a registered Rhai type must be `Clone`, and a `Box<dyn ProbaModel>` is not. The `Arc` clone is
/// a refcount bump, so a script assigning a fit to a second variable does not refit anything.
///
/// The two artifacts ride ALONG rather than being fetched later, because
/// [`vike_ml::Learner::fit_captured`]'s contract is ONE fit however many artifacts are asked for —
/// a `fit.importance()` that went back for a second fit would be the exact cost that method exists
/// to avoid.
#[derive(Clone)]
pub struct StudyFit {
    model: Arc<dyn ProbaModel>,
    importance: Option<FitImportance>,
    text: Option<String>,
}

// ---------------------------------------------------------------------------------------------
// the engine
// ---------------------------------------------------------------------------------------------

/// A fresh engine carrying the whole study surface, and NOTHING ELSE.
///
/// ⚠ **`set_module_resolver` is the load-bearing line in this function, not a tidy-up.**
/// `rhai::Engine::new()` installs a `FileModuleResolver` rooted at the process's working directory
/// (rhai-1.25.1 `src/engine.rs`'s `new`), so `import "…" as m;` READS A FILE FROM DISK on a default
/// engine. That is a file verb the Rust tier's sanctioned surface does not have, which
/// `docs/decisions/0029-a-study-reads-the-store-never-a-vendor-api.md` refuses — and it is
/// reachable without registering anything at all, which is exactly the kind of capability an
/// allow-list cannot see. `DummyModuleResolver` makes every `import` a refusal;
/// `crates/vike-studio-core/tests/rhai_study_pipeline.rs`'s
/// `a_study_cannot_import_a_module_because_that_would_be_a_file_verb` is the gate.
pub(super) fn build_engine(faults: &FaultLog) -> rhai::Engine {
    let mut engine = rhai::Engine::new();
    engine.set_module_resolver(rhai::module_resolvers::DummyModuleResolver::new());
    engine.set_max_operations(MAX_OPERATIONS);
    engine.set_max_call_levels(MAX_CALL_LEVELS);
    // Never stdout: see the module doc. `tracing` is the workspace's one logging facade and a
    // library uses it rather than choosing a stream for its caller.
    engine.on_print(|s| tracing::info!(target: "vike_study", "{s}"));
    engine.on_debug(|s, source, pos| {
        tracing::debug!(target: "vike_study", source = source.unwrap_or(""), position = %pos, "{s}");
    });

    register_types(&mut engine);
    register_rows(&mut engine);
    register_outcome(&mut engine);
    register_reads(&mut engine, faults);
    register_fit(&mut engine, faults);
    engine
}

/// Every type a study can hold a value of. A name is given explicitly so a rhai `type_of()` and
/// every error message read as the Rust type rather than as a mangled path.
fn register_types(engine: &mut rhai::Engine) {
    engine.register_type_with_name::<StudyContext>("StudyContext");
    engine.register_type_with_name::<StudyOutcome>("StudyOutcome");
    engine.register_type_with_name::<StudyFit>("StudyFit");
    engine.register_type_with_name::<TsRange>("TsRange");
    engine.register_type_with_name::<Bar>("Bar");
    engine.register_type_with_name::<QuoteTick>("QuoteTick");
    engine.register_type_with_name::<TradeTick>("TradeTick");
    engine.register_type_with_name::<BookUpdate>("BookUpdate");
}

// ---------------------------------------------------------------------------------------------
// the row types and their columns
// ---------------------------------------------------------------------------------------------

/// Which row type an array holds — the one fact [`column`] needs before it can project anything.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum RowKind {
    Bar,
    Quote,
    Trade,
    Book,
}

impl RowKind {
    /// The type's Rhai name, for an error message.
    fn label(self) -> &'static str {
        match self {
            RowKind::Bar => "Bar",
            RowKind::Quote => "QuoteTick",
            RowKind::Trade => "TradeTick",
            RowKind::Book => "BookUpdate",
        }
    }

    /// Every field name [`column`] projects for this row type.
    ///
    /// ⚠ It does NOT list `bids`/`asks`: those are per-row LEVEL LISTS, not scalars, so a column
    /// of them is not a column. They stay reachable through the property getter, which is where a
    /// nested value belongs.
    fn columns(self) -> &'static [&'static str] {
        match self {
            RowKind::Bar => {
                &["ts", "open", "high", "low", "close", "volume", "funding", "bid", "ask", "symbol"]
            }
            RowKind::Quote => &["ts", "local_ts", "bid", "ask", "bid_size", "ask_size", "symbol"],
            RowKind::Trade => &["ts", "local_ts", "price", "size", "is_buyer_maker", "symbol"],
            RowKind::Book => &["ts", "local_ts", "seq", "kind", "tick_size", "symbol"],
        }
    }
}

/// Which row type this value is, or `None` for anything a read verb did not produce.
fn row_kind(d: &Dynamic) -> Option<RowKind> {
    if d.is::<Bar>() {
        Some(RowKind::Bar)
    } else if d.is::<QuoteTick>() {
        Some(RowKind::Quote)
    } else if d.is::<TradeTick>() {
        Some(RowKind::Trade)
    } else if d.is::<BookUpdate>() {
        Some(RowKind::Book)
    } else {
        None
    }
}

/// `Option<f64>` as a float column entry — see [`field_of`] for why NaN and not `0.0`.
fn opt_f(v: Option<f64>) -> Dynamic {
    Dynamic::from(v.unwrap_or(f64::NAN))
}

/// One field of one row, or `None` when the value is not of `kind` (a MIXED array — the field name
/// itself is validated against [`RowKind::columns`] before the projection starts).
///
/// ⚠ **An absent optional reads as `NaN`, not as `0.0`.** `Bar::funding`/`bid`/`ask` are
/// `Option<f64>`; a float column's spelling of "there was no value here" is NaN, and it is the one
/// spelling a consumer cannot mistake for an observation. `0.0` is the `unwrap_or` that turns a gap
/// into a number, which is the failure `vike_user_research::StudyOutcome::metric`'s own doc refuses
/// for the same reason. An absent `symbol` reads as `()`, because a string column has no NaN.
fn field_of(kind: RowKind, d: &Dynamic, field: &str) -> Option<Dynamic> {
    match kind {
        RowKind::Bar => {
            let b = d.read_lock::<Bar>()?;
            Some(match field {
                "ts" => Dynamic::from(b.ts),
                "open" => Dynamic::from(b.open),
                "high" => Dynamic::from(b.high),
                "low" => Dynamic::from(b.low),
                "close" => Dynamic::from(b.close),
                "volume" => Dynamic::from(b.volume),
                "funding" => opt_f(b.funding),
                "bid" => opt_f(b.bid),
                "ask" => opt_f(b.ask),
                "symbol" => b.symbol.clone().map_or(Dynamic::UNIT, Dynamic::from),
                _ => return None,
            })
        }
        RowKind::Quote => {
            let q = d.read_lock::<QuoteTick>()?;
            Some(match field {
                "ts" => Dynamic::from(q.ts),
                "local_ts" => Dynamic::from(q.local_ts),
                "bid" => Dynamic::from(q.bid),
                "ask" => Dynamic::from(q.ask),
                "bid_size" => Dynamic::from(q.bid_size),
                "ask_size" => Dynamic::from(q.ask_size),
                "symbol" => Dynamic::from(q.symbol.clone()),
                _ => return None,
            })
        }
        RowKind::Trade => {
            let t = d.read_lock::<TradeTick>()?;
            Some(match field {
                "ts" => Dynamic::from(t.ts),
                "local_ts" => Dynamic::from(t.local_ts),
                "price" => Dynamic::from(t.price),
                "size" => Dynamic::from(t.size),
                "is_buyer_maker" => Dynamic::from(t.is_buyer_maker),
                "symbol" => Dynamic::from(t.symbol.clone()),
                _ => return None,
            })
        }
        RowKind::Book => {
            let u = d.read_lock::<BookUpdate>()?;
            Some(match field {
                "ts" => Dynamic::from(u.ts),
                "local_ts" => Dynamic::from(u.local_ts),
                "seq" => Dynamic::from(saturating_int(u.seq)),
                "kind" => Dynamic::from(book_kind(&u)),
                "tick_size" => Dynamic::from(u.tick_size),
                "symbol" => Dynamic::from(u.symbol.clone()),
                _ => return None,
            })
        }
    }
}

/// A `u64` count as a Rhai integer.
///
/// Rhai's `INT` is `i64` while `BookUpdate::seq` and `FitImportance::splits` are `u64`, so the top
/// half of the range has no spelling. It SATURATES rather than wrapping: a wrapped sequence reads
/// as a plausible small number and would silently reorder a replay, while `i64::MAX` is a value
/// nothing can mistake for a real one.
fn saturating_int(v: u64) -> i64 {
    i64::try_from(v).unwrap_or(i64::MAX)
}

/// One book event's kind as its Rust variant name — the spelling
/// `crates/vike-model/src/orderbook.rs`'s `BookUpdateKind` already has, so a study comparing
/// against `"Snapshot"` is comparing against the enum rather than against a second naming
/// convention invented here.
fn book_kind(u: &BookUpdate) -> String {
    format!("{:?}", u.kind)
}

/// One side of a book event as an array of `#{ price, size }` maps.
///
/// `vike_model::Level` is a bare `(f64, f64)` tuple, which Rhai would render as an opaque value
/// with no accessors at all — so the pair is named HERE, once, rather than every study having to
/// remember which element is the price.
fn levels(side: &[Level]) -> rhai::Array {
    side.iter()
        .map(|(price, size)| {
            let mut m = rhai::Map::new();
            m.insert("price".into(), Dynamic::from(*price));
            m.insert("size".into(), Dynamic::from(*size));
            Dynamic::from(m)
        })
        .collect()
}

/// Property getters for the four row types, plus [`column`] and the [`TsRange`] vocabulary.
fn register_rows(engine: &mut rhai::Engine) {
    engine.register_get("ts", |b: &mut Bar| b.ts);
    engine.register_get("open", |b: &mut Bar| b.open);
    engine.register_get("high", |b: &mut Bar| b.high);
    engine.register_get("low", |b: &mut Bar| b.low);
    engine.register_get("close", |b: &mut Bar| b.close);
    engine.register_get("volume", |b: &mut Bar| b.volume);
    engine.register_get("funding", |b: &mut Bar| b.funding.unwrap_or(f64::NAN));
    engine.register_get("bid", |b: &mut Bar| b.bid.unwrap_or(f64::NAN));
    engine.register_get("ask", |b: &mut Bar| b.ask.unwrap_or(f64::NAN));
    engine.register_get("symbol", |b: &mut Bar| {
        b.symbol.clone().map_or(Dynamic::UNIT, Dynamic::from)
    });

    engine.register_get("ts", |q: &mut QuoteTick| q.ts);
    engine.register_get("local_ts", |q: &mut QuoteTick| q.local_ts);
    engine.register_get("bid", |q: &mut QuoteTick| q.bid);
    engine.register_get("ask", |q: &mut QuoteTick| q.ask);
    engine.register_get("bid_size", |q: &mut QuoteTick| q.bid_size);
    engine.register_get("ask_size", |q: &mut QuoteTick| q.ask_size);
    engine.register_get("symbol", |q: &mut QuoteTick| q.symbol.clone());

    engine.register_get("ts", |t: &mut TradeTick| t.ts);
    engine.register_get("local_ts", |t: &mut TradeTick| t.local_ts);
    engine.register_get("price", |t: &mut TradeTick| t.price);
    engine.register_get("size", |t: &mut TradeTick| t.size);
    engine.register_get("is_buyer_maker", |t: &mut TradeTick| t.is_buyer_maker);
    engine.register_get("symbol", |t: &mut TradeTick| t.symbol.clone());

    engine.register_get("ts", |u: &mut BookUpdate| u.ts);
    engine.register_get("local_ts", |u: &mut BookUpdate| u.local_ts);
    engine.register_get("seq", |u: &mut BookUpdate| saturating_int(u.seq));
    engine.register_get("kind", |u: &mut BookUpdate| book_kind(u));
    engine.register_get("tick_size", |u: &mut BookUpdate| u.tick_size);
    engine.register_get("symbol", |u: &mut BookUpdate| u.symbol.clone());
    engine.register_get("bids", |u: &mut BookUpdate| levels(&u.bids));
    engine.register_get("asks", |u: &mut BookUpdate| levels(&u.asks));

    engine.register_fn("column", column);

    // ⚠ NOT `range`: rhai already registers a two-argument `range` iterator, and `register_fn`
    // REPLACES — see this module's doc.
    engine.register_fn("ts_range", TsRange::of);
    engine.register_fn("ts_range_all", TsRange::all);
    engine.register_get("start", |r: &mut TsRange| r.start.map_or(Dynamic::UNIT, Dynamic::from));
    engine.register_get("end", |r: &mut TsRange| r.end.map_or(Dynamic::UNIT, Dynamic::from));
}

/// One named field of every row in `rows`, as an array — the projection a real study reads its
/// data through.
///
/// ⚠ **This is the performance seam, and it is also the fit surface.** A Rhai `for` loop over a
/// hundred thousand rows spends interpreter operations per field access; the same read as ONE host
/// call is a `Vec` walk in Rust. It matters twice over, because the flat matrix [`register_fit`]
/// takes is built by concatenating columns — so the natural spelling of "fit on close and volume"
/// never materialises a per-row Rhai value at all.
///
/// An EMPTY array projects to an empty array: a study whose window held no rows asked a
/// well-formed question and got a well-formed answer, and refusing it would make every study write
/// the same guard.
///
/// Both refusals are LOUD, because both are silently-wrong-answer shapes otherwise: an unknown
/// field name would project a column of `()` that a study would then average, and a mixed array
/// would project whichever rows happened to match.
///
/// ⚠ Takes `rhai::ImmutableString` rather than `&str` so it can be registered as a FUNCTION ITEM.
/// Rhai accepts a `&str` parameter, but only on a closure whose lifetime rustc can pin — and a
/// forwarding closure is what `clippy::redundant_closure` refuses. This is the spelling that
/// satisfies both.
fn column(
    rows: rhai::Array,
    field: rhai::ImmutableString,
) -> Result<rhai::Array, Box<EvalAltResult>> {
    let field = field.as_str();
    let Some(first) = rows.first() else { return Ok(rhai::Array::new()) };
    let kind = row_kind(first).ok_or_else(|| -> Box<EvalAltResult> {
        format!(
            "column: this array holds `{}`, which is not a row a read verb produced. `column` \
             projects Bar / QuoteTick / TradeTick / BookUpdate.",
            first.type_name()
        )
        .into()
    })?;
    if !kind.columns().contains(&field) {
        return Err(format!(
            "column: `{}` has no field `{field}`. It has: {}.",
            kind.label(),
            kind.columns().join(", ")
        )
        .into());
    }
    let mut out = rhai::Array::with_capacity(rows.len());
    for (i, row) in rows.iter().enumerate() {
        let v = field_of(kind, row, field).ok_or_else(|| -> Box<EvalAltResult> {
            format!(
                "column: row {i} is a `{}` but row 0 is a `{}` — a mixed array would project only \
                 the rows that happened to match.",
                row.type_name(),
                kind.label()
            )
            .into()
        })?;
        out.push(v);
    }
    Ok(out)
}

// ---------------------------------------------------------------------------------------------
// the outcome
// ---------------------------------------------------------------------------------------------

/// The return value's constructor and its two mutators.
///
/// Both mutators surface the Rust `Result` as a Rhai RUNTIME ERROR rather than as a value, so a
/// refused metric or a refused artifact name stops the study instead of being dropped on the floor
/// by a script that never checks a return.
fn register_outcome(engine: &mut rhai::Engine) {
    engine.register_fn("outcome", StudyOutcome::new);

    // ⚠ `rhai::Dynamic`, NOT `f64`. Registered as `f64` this would accept `out.metric("n", 3.0)`
    // and REJECT `out.metric("n", 3)` with a rhai "function not found" naming an i64 — a knob that
    // works or not depending on whether the author typed a decimal point. That exact bug shipped
    // in this workspace once already, in `crates/vike-script/src/engine.rs`'s `param`.
    engine.register_fn(
        "metric",
        |out: &mut StudyOutcome, name: &str, value: Dynamic| -> Result<(), Box<EvalAltResult>> {
            let v = number(&value).ok_or_else(|| -> Box<EvalAltResult> {
                format!(
                    "metric `{name}`: a metric is a NUMBER and this is a `{}`. A study that wants \
                     to record text records an artifact.",
                    value.type_name()
                )
                .into()
            })?;
            out.metric(name, v).map_err(|e| -> Box<EvalAltResult> { e.to_string().into() })
        },
    );

    // Deliberately refuses a non-string body rather than rendering one. `StudyOutcome::artifact`
    // takes TEXT (its own doc argues the narrowing), and `to_string()`ing a number here would
    // write a one-character file for an author who meant to pass the table they had built.
    engine.register_fn(
        "artifact",
        |out: &mut StudyOutcome, name: &str, body: Dynamic| -> Result<(), Box<EvalAltResult>> {
            let text = body.into_immutable_string().map_err(|got| -> Box<EvalAltResult> {
                format!("artifact `{name}`: an artifact body is TEXT and this is a `{got}`.").into()
            })?;
            out.artifact(name, text.to_string())
                .map_err(|e| -> Box<EvalAltResult> { e.to_string().into() })
        },
    );
}

/// One Rhai value as a number, accepting both of rhai's numeric types. `None` for anything else —
/// deliberately NOT a fall-through to zero or NaN, which the caller turns into a raised error.
fn number(v: &Dynamic) -> Option<f64> {
    v.as_float().ok().or_else(|| v.as_int().ok().map(|i| i as f64))
}

// ---------------------------------------------------------------------------------------------
// the read verbs
// ---------------------------------------------------------------------------------------------

/// [`StudyContext`]'s read verbs, one Rhai method per Rust method, each in two arities.
///
/// The short arity uses [`StudyContext::window`] — the run's own window, which is what a study
/// wants unless it genuinely needs history BEFORE it (that method's doc says so). Making the
/// common case the SHORT one is what stops a study from writing its own window out of `params` and
/// quietly measuring a different range than the run it is filed under.
///
/// `ctx.scratch()` is deliberately ABSENT — see [`super`]'s module doc for the argument.
fn register_reads(engine: &mut rhai::Engine, faults: &FaultLog) {
    engine.register_fn("window", |ctx: &mut StudyContext| ctx.window());
    engine.register_fn("can_fit", |ctx: &mut StudyContext| ctx.learner().is_some());

    let f = faults.clone();
    engine.register_fn(
        "bars",
        move |ctx: &mut StudyContext, venue: &str, symbol: &str, interval: &str| {
            let w = ctx.window();
            rows(ctx.bars(venue, symbol, interval, w), &f)
        },
    );
    let f = faults.clone();
    engine.register_fn(
        "bars",
        move |ctx: &mut StudyContext, venue: &str, symbol: &str, interval: &str, range: TsRange| {
            rows(ctx.bars(venue, symbol, interval, range), &f)
        },
    );

    macro_rules! read_verb {
        ($name:literal, $call:ident) => {
            let f = faults.clone();
            engine.register_fn($name, move |ctx: &mut StudyContext, venue: &str, symbol: &str| {
                let w = ctx.window();
                rows(ctx.$call(venue, symbol, w), &f)
            });
            let f = faults.clone();
            engine.register_fn(
                $name,
                move |ctx: &mut StudyContext, venue: &str, symbol: &str, range: TsRange| {
                    rows(ctx.$call(venue, symbol, range), &f)
                },
            );
        };
    }
    read_verb!("quotes", quotes);
    read_verb!("trades", trades);
    read_verb!("book_updates", book_updates);
    read_verb!("depth", depth);
}

/// One store read's rows as a Rhai array, or the read's own failure RAISED with its typed variant
/// preserved — see [`super::fault`] for how a `StudyError::Data` survives the interpreter.
fn rows<T: Clone + Send + Sync + 'static>(
    read: Result<Vec<T>, vike_data::DataError>,
    faults: &FaultLog,
) -> Result<rhai::Array, Box<EvalAltResult>> {
    match read {
        Ok(v) => Ok(v.into_iter().map(Dynamic::from).collect()),
        Err(e) => Err(raise(faults, StudyError::Data(e))),
    }
}

// ---------------------------------------------------------------------------------------------
// the fitting surface
// ---------------------------------------------------------------------------------------------

/// Every key `ctx.fit`'s parameter map accepts.
///
/// ⚠ **An unknown key is REFUSED**, which is the whole reason this list exists as data. A silently
/// ignored `learning_Rate` is a knob the author believes is armed and is not — the defect class
/// `vike_config::CONSUMPTION` exists for, in a place no settings gate can see. The five axes are
/// [`vike_ml::GridPoint`]'s own field names, so an author reading LightGBM's documentation types
/// what it says.
pub const FIT_PARAM_KEYS: &[&str] = &[
    "num_leaves",
    "max_depth",
    "min_data_in_leaf",
    "learning_rate",
    "feature_fraction",
    "seed",
    "capture_importance",
    "capture_text",
];

/// The fitting surface: `ctx.fit(x, y, n_cols, params)` and the [`StudyFit`] it returns.
///
/// The argument for this signature is in [`super`]'s module doc — briefly: `TrainData<'_>` is
/// BORROWED and `GridPoint` is a five-field struct, so neither is constructible from a script;
/// this verb takes the two flat arrays and a map, builds both INSIDE the host, and hands back an
/// opaque handle.
fn register_fit(engine: &mut rhai::Engine, faults: &FaultLog) {
    let f = faults.clone();
    engine.register_fn(
        "fit",
        move |ctx: &mut StudyContext,
              x: rhai::Array,
              y: rhai::Array,
              n_cols: i64,
              params: rhai::Map|
              -> Result<StudyFit, Box<EvalAltResult>> {
            fit(ctx, &x, &y, n_cols, &params, &f)
        },
    );

    engine.register_fn("has_text", |fit: &mut StudyFit| fit.text.is_some());
    engine.register_fn("has_importance", |fit: &mut StudyFit| fit.importance.is_some());

    engine.register_fn(
        "predict",
        |fit: &mut StudyFit, row: rhai::Array| -> Result<f64, Box<EvalAltResult>> {
            let r = floats(&row, "predict: row")?;
            Ok(fit.model.predict_proba(&r))
        },
    );

    // Both artifact accessors RAISE when the learner reported nothing, rather than answering an
    // empty value. `vike_ml::Learner::fit_captured`'s own doc makes the asymmetry the CALLER's
    // decision and states which way it falls: an empty importance table positively misinforms
    // ("no feature mattered"), so a study must not be able to write one by accident. The text half
    // is symmetric here for one reason — an empty `model.txt` is an artifact somebody would later
    // try to load.
    engine.register_fn("text", |fit: &mut StudyFit| -> Result<String, Box<EvalAltResult>> {
        fit.text.clone().ok_or_else(|| -> Box<EvalAltResult> {
            "fit.text(): this fit captured no model text. Ask for it with `capture_text: true`, \
             and check `fit.has_text()` — a learner that cannot serialise itself answers none even \
             when asked."
                .into()
        })
    });

    engine.register_fn(
        "importance",
        |fit: &mut StudyFit| -> Result<rhai::Array, Box<EvalAltResult>> {
            let imp = fit.importance.as_ref().ok_or_else(|| -> Box<EvalAltResult> {
                "fit.importance(): this fit captured no importance table. Ask for it with \
                 `capture_importance: true`, and check `fit.has_importance()` — a learner that \
                 cannot report one answers none even when asked. An EMPTY table is not returned in \
                 its place: it would read as `no feature mattered`."
                    .into()
            })?;
            Ok(imp
                .gain
                .iter()
                .zip(imp.splits.iter())
                .enumerate()
                .map(|(i, (gain, splits))| {
                    let mut m = rhai::Map::new();
                    m.insert("feature".into(), Dynamic::from(i as i64));
                    m.insert("gain".into(), Dynamic::from(*gain));
                    m.insert("splits".into(), Dynamic::from(saturating_int(*splits)));
                    Dynamic::from(m)
                })
                .collect())
        },
    );
}

/// `ctx.fit`, as a function so the registration above stays readable.
fn fit(
    ctx: &StudyContext,
    x: &rhai::Array,
    y: &rhai::Array,
    n_cols: i64,
    params: &rhai::Map,
    faults: &FaultLog,
) -> Result<StudyFit, Box<EvalAltResult>> {
    let learner = ctx.learner().ok_or_else(|| {
        raise(faults, StudyError::NoLearner(format!("a {}-column matrix", n_cols.max(0))))
    })?;

    let (point, seed, want) = fit_params(params)?;
    let xs = floats(x, "fit: x")?;
    let ys: Vec<f32> = floats(y, "fit: y")?.into_iter().map(|v| v as f32).collect();
    let n_cols = usize::try_from(n_cols).map_err(|_| -> Box<EvalAltResult> {
        format!("fit: n_cols must be a positive whole number, got {n_cols}").into()
    })?;

    // `TrainData::new` validates the shape HERE rather than letting a trainer discover it forty
    // lines into a CSV — the reason that constructor exists. Its refusal is the study author's to
    // fix, so it is an ordinary rhai error rather than a typed fault.
    let data = TrainData::new(&xs, &ys, n_cols)
        .map_err(|e| -> Box<EvalAltResult> { format!("fit: {e}").into() })?;

    let (model, importance, text) = learner
        .fit_captured(&data, &point, seed, want)
        .map_err(|e| -> Box<EvalAltResult> { format!("fit refused: {e}").into() })?;
    Ok(StudyFit { model: Arc::from(model), importance, text })
}

/// The parameter map as the three things a fit actually takes. Every key is optional; an unknown
/// one is refused by name (see [`FIT_PARAM_KEYS`]).
fn fit_params(params: &rhai::Map) -> Result<(GridPoint, u64, Capture), Box<EvalAltResult>> {
    for k in params.keys() {
        if !FIT_PARAM_KEYS.contains(&k.as_str()) {
            return Err(format!(
                "fit: unknown parameter `{k}`. Accepted: {}.",
                FIT_PARAM_KEYS.join(", ")
            )
            .into());
        }
    }
    let get = |name: &str| params.iter().find(|(k, _)| k.as_str() == name).map(|(_, v)| v);
    let num = |name: &str| -> Result<Option<f64>, Box<EvalAltResult>> {
        match get(name) {
            None => Ok(None),
            Some(v) => number(v).map(Some).ok_or_else(|| -> Box<EvalAltResult> {
                format!("fit: `{name}` must be a number, got a `{}`", v.type_name()).into()
            }),
        }
    };
    let flag = |name: &str| -> Result<bool, Box<EvalAltResult>> {
        match get(name) {
            None => Ok(false),
            Some(v) => v.as_bool().map_err(|got| -> Box<EvalAltResult> {
                format!("fit: `{name}` must be true or false, got a `{got}`").into()
            }),
        }
    };

    let mut point = DEFAULT_POINT;
    if let Some(v) = num("num_leaves")? {
        point.num_leaves = v as u32;
    }
    if let Some(v) = num("max_depth")? {
        point.max_depth = v as i32;
    }
    if let Some(v) = num("min_data_in_leaf")? {
        point.min_data_in_leaf = v as u32;
    }
    if let Some(v) = num("learning_rate")? {
        point.learning_rate = v;
    }
    if let Some(v) = num("feature_fraction")? {
        point.feature_fraction = v;
    }
    // Negative is clamped rather than refused: a seed is an arbitrary label, and `u64::MAX` for a
    // typo'd `-1` would be a different fit under a plausible-looking spelling.
    let seed = num("seed")?.unwrap_or(0.0).max(0.0) as u64;
    let want = Capture { importance: flag("capture_importance")?, text: flag("capture_text")? };
    Ok((point, seed, want))
}

/// A Rhai array as `Vec<f64>`, refusing a non-numeric entry by INDEX.
///
/// `what` names the argument so `fit: y` and `predict: row` do not read as one another. Accepts
/// integers as well as floats for the reason [`number`] gives.
fn floats(a: &rhai::Array, what: &str) -> Result<Vec<f64>, Box<EvalAltResult>> {
    let mut out = Vec::with_capacity(a.len());
    for (i, v) in a.iter().enumerate() {
        out.push(number(v).ok_or_else(|| -> Box<EvalAltResult> {
            format!("{what}[{i}] must be a number, got a `{}`", v.type_name()).into()
        })?);
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn a_bar() -> Bar {
        Bar {
            ts: 7,
            open: 1.0,
            high: 2.0,
            low: 0.5,
            close: 1.5,
            volume: 10.0,
            funding: None,
            bid: None,
            ask: None,
            symbol: None,
        }
    }

    fn a_quote() -> QuoteTick {
        QuoteTick {
            ts: 1,
            local_ts: 2,
            bid: 1.0,
            ask: 1.1,
            bid_size: 3.0,
            ask_size: 4.0,
            symbol: "BTCUSDT".into(),
        }
    }

    fn a_trade() -> TradeTick {
        TradeTick {
            ts: 1,
            local_ts: 2,
            price: 1.0,
            size: 3.0,
            is_buyer_maker: true,
            symbol: "BTCUSDT".into(),
        }
    }

    fn a_book() -> BookUpdate {
        BookUpdate {
            ts: 1,
            local_ts: 2,
            seq: 9,
            kind: vike_model::BookUpdateKind::Snapshot,
            tick_size: 0.01,
            bids: vec![(1.0, 2.0)],
            asks: vec![(1.1, 3.0)],
            symbol: "BTCUSDT".into(),
        }
    }

    /// [`RowKind::columns`] is what an error message advertises and [`field_of`] is what actually
    /// projects. A name in one and not the other is a silent `()` column or an unreachable field,
    /// so the two are checked against each other rather than trusted to stay in step.
    #[test]
    fn every_advertised_column_actually_projects_and_nothing_else_does() {
        let rows: [(RowKind, Dynamic); 4] = [
            (RowKind::Bar, Dynamic::from(a_bar())),
            (RowKind::Quote, Dynamic::from(a_quote())),
            (RowKind::Trade, Dynamic::from(a_trade())),
            (RowKind::Book, Dynamic::from(a_book())),
        ];
        for (kind, row) in &rows {
            for field in kind.columns() {
                assert!(
                    field_of(*kind, row, field).is_some(),
                    "{}.{field} is advertised but does not project",
                    kind.label()
                );
            }
            assert!(
                field_of(*kind, row, "definitely_not_a_field").is_none(),
                "{} projected a field it does not have",
                kind.label()
            );
            assert_eq!(row_kind(row), Some(*kind));
        }
        assert_eq!(row_kind(&Dynamic::from(1_i64)), None, "a plain int is not a row");
    }

    /// `bids`/`asks` are level LISTS, not scalars — reachable as properties, never as a column. If
    /// they ever joined the table, `column(rows, "bids")` would build an array of arrays that
    /// nothing downstream (the fit matrix above all) could consume.
    #[test]
    fn the_level_lists_are_properties_and_not_columns() {
        for f in ["bids", "asks"] {
            assert!(!RowKind::Book.columns().contains(&f));
        }
        assert_eq!(levels(&[(1.0, 2.0)]).len(), 1);
    }

    /// An absent optional is NaN in a float column — never `0.0`, which reads as an observation.
    #[test]
    fn an_absent_optional_projects_as_nan_rather_than_zero() {
        let row = Dynamic::from(a_bar());
        for f in ["funding", "bid", "ask"] {
            let v = field_of(RowKind::Bar, &row, f).unwrap();
            assert!(v.as_float().unwrap().is_nan(), "{f} projected {v:?}");
        }
        assert!(field_of(RowKind::Bar, &row, "symbol").unwrap().is_unit());
    }

    #[test]
    fn a_count_past_the_rhai_integer_saturates_rather_than_wrapping() {
        assert_eq!(saturating_int(9), 9);
        assert_eq!(saturating_int(u64::MAX), i64::MAX);
    }

    #[test]
    fn column_refuses_an_unknown_field_and_a_mixed_array_by_naming_both() {
        let bars = vec![Dynamic::from(a_bar())];
        let e = column(bars.clone(), "clsoe".into()).unwrap_err().to_string();
        assert!(e.contains("clsoe") && e.contains("close"), "{e}");

        let mixed = vec![Dynamic::from(a_bar()), Dynamic::from(a_quote())];
        let e = column(mixed, "ts".into()).unwrap_err().to_string();
        assert!(e.contains("row 1") && e.contains("QuoteTick"), "{e}");

        let e = column(vec![Dynamic::from(1_i64)], "ts".into()).unwrap_err().to_string();
        assert!(e.contains("not a row"), "{e}");

        assert!(
            column(rhai::Array::new(), "anything".into()).unwrap().is_empty(),
            "empty in, empty out"
        );
        assert_eq!(column(bars, "close".into()).unwrap()[0].as_float().unwrap(), 1.5);
    }

    #[test]
    fn an_unknown_fit_parameter_is_refused_by_name_rather_than_ignored() {
        let mut m = rhai::Map::new();
        m.insert("learning_Rate".into(), Dynamic::from(0.1_f64));
        let e = fit_params(&m).unwrap_err().to_string();
        assert!(e.contains("learning_Rate") && e.contains("learning_rate"), "{e}");
    }

    /// Both of rhai's numeric types reach every axis: `num_leaves: 31` and `num_leaves: 31.0` must
    /// mean the same thing, which is the `param`-shaped trap this workspace has already shipped
    /// once.
    #[test]
    fn a_fit_parameter_takes_an_integer_and_a_float_alike() {
        let mut m = rhai::Map::new();
        m.insert("num_leaves".into(), Dynamic::from(31_i64));
        m.insert("learning_rate".into(), Dynamic::from(0.25_f64));
        m.insert("seed".into(), Dynamic::from(7_i64));
        m.insert("capture_text".into(), Dynamic::from(true));
        let (p, seed, want) = fit_params(&m).unwrap();
        assert_eq!(p.num_leaves, 31);
        assert_eq!(p.learning_rate, 0.25);
        assert_eq!(p.max_depth, DEFAULT_POINT.max_depth, "an unset axis keeps the default point");
        assert_eq!(seed, 7);
        assert_eq!(want, Capture { importance: false, text: true });

        let mut m = rhai::Map::new();
        m.insert("num_leaves".into(), Dynamic::from(31.0_f64));
        assert_eq!(fit_params(&m).unwrap().0.num_leaves, 31);
    }

    #[test]
    fn a_non_numeric_fit_parameter_is_refused_rather_than_coerced() {
        let mut m = rhai::Map::new();
        m.insert("learning_rate".into(), Dynamic::from("0.1"));
        let e = fit_params(&m).unwrap_err().to_string();
        assert!(e.contains("learning_rate") && e.contains("must be a number"), "{e}");

        let mut m = rhai::Map::new();
        m.insert("capture_text".into(), Dynamic::from(1_i64));
        assert!(fit_params(&m).unwrap_err().to_string().contains("true or false"));
    }

    #[test]
    fn a_non_numeric_matrix_entry_is_refused_by_index() {
        let a = vec![Dynamic::from(1.0_f64), Dynamic::from("nope")];
        let e = floats(&a, "fit: x").unwrap_err().to_string();
        assert!(e.contains("fit: x[1]"), "{e}");
        let one = vec![Dynamic::from(2_i64)];
        assert_eq!(floats(&one, "fit: x").unwrap(), vec![2.0]);
    }
}
