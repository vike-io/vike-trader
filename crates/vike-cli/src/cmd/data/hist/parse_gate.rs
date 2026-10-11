//! `data hist gate` — the arm of the per-subcommand `match` in `super::parse` that holds its
//! grammar.
//!
//! Moved here verbatim from that function (code-layout phase 2, task 11); see
//! `parse_fetch` for how the pieces fit. The module this verb's pure half lives in is `gate`, which
//! is why this file is not simply called that.

use super::{
    Filter, GateArgs, Sub, Window, gate, refuse_an_account_kind_on_a_read, refuse_foreign_flags,
};

/// `gate`'s arm: resolves the spec and the criteria, so the judging is a fold over plain data.
/// Returns the `(spec, window)` pair `parse` builds its `Args` from, and the `GateArgs` it carries.
#[expect(clippy::type_complexity)] // the arm's `(spec, window)` pair plus the struct it builds
pub(super) fn parse(
    sub: Sub,
    filter: &Filter,
    class: bool,
    partial_only: bool,
    spec: Option<String>,
    require_days: Option<String>,
    max_gap: Option<String>,
    require_kinds: Vec<String>,
) -> Result<(Option<String>, Option<Window>, Option<GateArgs>), String> {
    let gate_args;
    let (spec, window) = {
        // The browse aids are refused with the reason this verb makes them wrong rather than
        // merely inapplicable: a gate ASSERTS, and every one of these three widens or annotates
        // a listing. `--kind` is the one an operator will reach for first, so its message names
        // the criterion flag that replaced it rather than the shape of the mistake.
        refuse_foreign_flags(
            sub,
            &[("--venue", filter.venue.is_some()), ("--name", filter.name.is_some())],
            "that flag narrows a LISTING by substring, and a gate names the series it asserts \
                 about EXACTLY — in the spec. A substring match would pass the gate on a series \
                 you did not name, which is a green build over the wrong data",
        )?;
        refuse_foreign_flags(
            sub,
            &[("--kind", filter.kind.is_some())],
            "that flag filters a listing; here the kind is a CRITERION, so it is \
                 `--require-kind K` — repeatable, defaulting to `bar`, and a kind named there that \
                 the store lacks is a BREACH rather than a row that quietly vanished",
        )?;
        refuse_foreign_flags(
            sub,
            &[("--class", class), ("--partial-only", partial_only)],
            "that flag annotates a LISTING, and every line here is a criterion and its \
                 verdict. A recorded asset class is not something a store can be READY or not \
                 ready for, and it costs a round trip per instrument to fetch — \
                 `vike-cli data hist ls --class` is where it is",
        )?;
        let spec = spec.ok_or(
            "gate needs a spec: VENUE:SYMBOL[:INTERVAL], or VENUE:@GROUP for a grouped \
                 series — e.g. `vike-cli data hist gate binance:BTCUSDT:1h --require-days 365 \
                 --max-gap 1d`. A gate over `whatever the store holds` is a gate nobody can act on",
        )?;
        let spec = gate::parse_spec(&spec)?;
        // ⚠ REQUIRED, and refused at the door rather than defaulted. The precedent is
        // `crates/vike-cli/src/cmd/runs/gate.rs`'s `refuse_an_ungateable_line`: a gate with no
        // criteria would exit 0 having checked nothing, which is the one answer a CI step must
        // never get — so there is no default that could be safe here.
        // ⚠ The remedy clause says what `--require-days 1` IS, and it used to say something
        // else: "For a PRESENCE-only gate write `--require-days 1`". That label was false —
        // `judge_days` requires `span_ms >= 86_400_000`, so a series that is present and six
        // hours old BREACHES it, and an operator who wrote the advertised line into an
        // `ExecStartPre=` had a unit refusing to start over exactly the tape it asked for. The
        // trailing clause was always accurate; the label in front of it is the part a reader
        // skims, so the label is gone and the absence it papered over is stated outright.
        let require_days = require_days.ok_or(
            "gate needs --require-days N: a gate with no criterion exits 0 having checked \
                 nothing, which is the one answer a CI step must never get. There is no \
                 PRESENCE-only spelling — the narrowest gate is `--require-days 1`, which asserts \
                 the series holds a WHOLE DAY of anything, so a tape fetched an hour ago breaches \
                 it",
        )?;
        let require_days = gate::parse_require_days(&require_days)?;
        let max_gap_ms = max_gap.as_deref().map(gate::parse_max_gap).transpose()?;
        // ⚠ The account-kind REFUSAL is applied HERE, at PARSE time, where `ls` applies its
        // twin at execute time. Both are right for their verb: a listing's `--kind` is a filter
        // over an answer that has already arrived, while a gate's criterion decides whether a
        // socket is worth opening at all — and a criterion that can never be honoured must not
        // cost a connection to discover ([`refuse_a_blank_produced_by`] makes the same trade).
        //
        // ⚠ It is HALF of §9.3.2 and was once mistaken for the whole of it: the other half is
        // the EXCLUSION, which this verb owes exactly as `ls` does and which lives in
        // [`execute_gate`] because it is about what the store ANSWERED, not about what the
        // operator asked. A refused criterion says nothing about evidence.
        let mut kinds = Vec::new();
        for kind in require_kinds {
            let kind = kind.trim().to_string();
            if kind.is_empty() {
                return Err(
                    "--require-kind was given an EMPTY value. It names a kind the store must \
                         hold (bar/quote/trade/book/depth and more); omit the flag to gate `bar`, \
                         which is the default"
                        .to_string(),
                );
            }
            refuse_an_account_kind_on_a_read(&kind)?;
            // Repeating a kind is accepted and collapsed rather than refused: the flag
            // declares a SET, and a set that already contains the value is not a mistake worth
            // a message. Collapsing keeps the verdict one row per kind.
            if !kinds.contains(&kind) {
                kinds.push(kind);
            }
        }
        if kinds.is_empty() {
            kinds.push(gate::DEFAULT_KIND.to_string());
        }
        // ⚠ AFTER the default is folded in, because the default is the one kind a spec's third
        // part can select — checking before it would refuse nothing and check the wrong set.
        // The refusal itself is the mirror of `gate::parse_spec`'s grouped one; see its doc.
        gate::refuse_a_kind_the_spec_can_never_select(&spec, &kinds)?;
        gate_args = Some(GateArgs { spec, require_days, max_gap_ms, kinds });
        (None, None)
    };
    Ok((spec, window, gate_args))
}
