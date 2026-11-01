//! `data hist get` — the arm of the per-subcommand `match` in `super::parse` that holds its
//! grammar.
//!
//! Moved here verbatim from that function (code-layout phase 2, task 11); see
//! `parse_fetch` for how the pieces fit. The module this verb's pure half lives in is `get`, which
//! is why this file is not simply called that.

use super::{
    Filter, GetArgs, Sub, Window, get, refuse_an_account_kind_on_a_read, refuse_foreign_flags,
};

/// `get`'s arm: resolves the spec, the REQUIRED window and the row ceiling before a socket is
/// opened. Returns the `(spec, window)` pair `parse` builds its `Args` from, and the `GetArgs` it
/// carries.
#[expect(clippy::type_complexity)] // the arm's `(spec, window)` pair plus the struct it builds
pub(super) fn parse(
    sub: Sub,
    filter: &Filter,
    class: bool,
    partial_only: bool,
    spec: Option<String>,
    days: Option<String>,
    from: Option<String>,
    to: Option<String>,
    limit: Option<String>,
    get_render: Option<get::Render>,
) -> Result<(Option<String>, Option<Window>, Option<GetArgs>), String> {
    let get_args;
    let (spec, window) = {
        // ⚠ **THE ACCOUNT-KIND REFUSAL COMES FIRST, and the ORDER is the point.** This verb
        // takes no `--kind` at all, so the obvious answer to one is "that flag belongs to a
        // listing". For an ACCOUNT kind that answer would be a fact about this verb's shape
        // standing in front of a fact about the PLANE — §9.3.2's rule is that your fills,
        // orders, funding payments and equity are not `data`'s to serve, and an operator who
        // asked for them must meet that sentence rather than a flag-placement note. It is the
        // same sentence `ls --kind` and `gate --require-kind` give, from the one function that
        // spells it ([`refuse_an_account_kind_on_a_read`]).
        if let Some(kind) = filter.kind.as_deref() {
            refuse_an_account_kind_on_a_read(kind)?;
            return Err(format!(
                "--kind does not apply to `get`: it reads BARS, and the bar step is the \
                     spec's THIRD part ({kind:?} would be a different row shape — a quote, a \
                     print or a book level — which this verb does not serve). \
                     `vike-cli data hist ls --kind {kind}` lists what the store holds of it"
            ));
        }
        refuse_foreign_flags(
            sub,
            &[
                ("--venue", filter.venue.is_some()),
                ("--name", filter.name.is_some()),
                ("--class", class),
                ("--partial-only", partial_only),
            ],
            "that flag narrows or annotates a LISTING, and this verb reads ONE series named \
                 EXACTLY by its spec — there is nothing here to filter. `vike-cli data hist ls` \
                 is the verb those flags belong to",
        )?;
        let spec = spec.ok_or(
            "get needs a spec: VENUE:SYMBOL:INTERVAL, e.g. `vike-cli data hist get \
                 binance:BTCUSDT:1h --days 7 --limit 20`. This verb reads ONE bar series; \
                 `vike-cli data hist ls` is how you find out which ones a store holds",
        )?;
        let spec = get::parse_spec(&spec)?;
        // ⚠ §8.2's cost guard, both halves, at PARSE time — before a socket is opened, for
        // `gate`'s stated reason: nothing here is forwarded, so the far side could never
        // refuse it, and a request that can never be honoured must not cost a connection to
        // discover.
        let window = get::parse_window(days.as_deref(), from.as_deref(), to.as_deref())?;
        let limit_defaulted = limit.is_none();
        let limit = get::parse_limit(limit.as_deref())?;
        get_args = Some(GetArgs {
            spec,
            window,
            limit,
            limit_defaulted,
            // Present by construction: the match above builds one for every `Sub::Get`.
            render: get_render.expect("`parse` resolves a Render for every Sub::Get"),
        });
        (None, None)
    };
    Ok((spec, window, get_args))
}
