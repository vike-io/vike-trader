//! `hist_store_stubs!` — the REQUIRED `HistStore` verbs a test double is not about, in one line.
//!
//! The macro carries the manual (it is the exported item; this module is private). Its unit tests
//! are `stubs_tests.rs`, beside this file.

/// The REQUIRED `HistStore` verbs a test double is not about, in one line.
///
/// Every hand-written double has to spell the trait's 18 required methods, and almost every one
/// carries one to nine verbs of real behaviour (seeded rows, a scripted answer, a counter, a panic)
/// plus a stub for each of the rest. The stubs come in exactly two flavours, so the macro has two:
///
/// - `inert` — a read answers `Ok` with an empty `Vec`, a write or a resample answers `Ok(0)`;
/// - `refuse(f)` — every stub answers `Err(f("<verb>"))`, where `f` is the double's own
///   `Fn(&'static str) -> DataError`, so a verb reached off the test's walk says WHICH verb it was.
///
/// Invoke it INSIDE `impl HistStore for X { … }`, after the verbs the double exists for. Each item
/// is a required verb by name, or a group: `reads` (the eight required reads: `load_bars` and the
/// seven `scan_*`), `writes` (the eight required `append_*` and the two `resample_*_to_bars`), or
/// `all` (both):
///
/// ```ignore
/// impl HistStore for CountingStore {
///     fn scan_quotes(/* … */) -> Result<Vec<QuoteTick>, DataError> { /* the verb under test */ }
///     vike_data::hist_store_stubs!(inert: writes, load_bars, scan_trades, scan_book_updates,
///         scan_symbol_properties, scan_equity, scan_exec_fills, scan_exec_orders);
/// }
/// impl HistStore for RefusingCatalogStore {
///     fn list_series(&self) -> Result<Vec<SeriesId>, DataError> { /* … */ }
///     vike_data::hist_store_stubs!(refuse(off_walk): all);
/// }
/// ```
///
/// What it does NOT do, on purpose:
///
/// - **It never stubs a DEFAULTED verb.** Those keep the trait's own body, the answer
///   `crates/vike-data/src/store/hist.rs` argues for each one (some refuse, some answer empty); a
///   double that needs another answer writes that verb by hand. Naming a defaulted verb here is a
///   compile error that says so.
/// - **It cannot subtract.** A verb the double writes by hand must not also be listed, or the impl
///   holds two definitions and rustc refuses it — which is the error you want.
///
/// The model types are spelled `::vike_model::…`, so the invoking crate names `vike-model` as a
/// dependency of its own (every `HistStore` implementor does: the trait's signatures are its types).
#[macro_export]
macro_rules! hist_store_stubs {
    (inert: $($item:ident),+ $(,)?) => {
        $( $crate::hist_store_stubs!(@item [inert] $item); )+
    };
    (refuse($err:expr): $($item:ident),+ $(,)?) => {
        $( $crate::hist_store_stubs!(@item [refuse $err] $item); )+
    };

    // ---- the groups -------------------------------------------------------------------------
    (@item $mode:tt all) => {
        $crate::hist_store_stubs!(@item $mode reads);
        $crate::hist_store_stubs!(@item $mode writes);
    };
    (@item $mode:tt reads) => {
        $crate::hist_store_stubs!(@body $mode load_bars);
        $crate::hist_store_stubs!(@body $mode scan_quotes);
        $crate::hist_store_stubs!(@body $mode scan_trades);
        $crate::hist_store_stubs!(@body $mode scan_book_updates);
        $crate::hist_store_stubs!(@body $mode scan_symbol_properties);
        $crate::hist_store_stubs!(@body $mode scan_equity);
        $crate::hist_store_stubs!(@body $mode scan_exec_fills);
        $crate::hist_store_stubs!(@body $mode scan_exec_orders);
    };
    (@item $mode:tt writes) => {
        $crate::hist_store_stubs!(@body $mode append_bars);
        $crate::hist_store_stubs!(@body $mode append_quotes);
        $crate::hist_store_stubs!(@body $mode append_trades);
        $crate::hist_store_stubs!(@body $mode append_book_updates);
        $crate::hist_store_stubs!(@body $mode append_symbol_properties);
        $crate::hist_store_stubs!(@body $mode append_equity);
        $crate::hist_store_stubs!(@body $mode append_exec_fills);
        $crate::hist_store_stubs!(@body $mode append_exec_orders);
        $crate::hist_store_stubs!(@body $mode resample_quotes_to_bars);
        $crate::hist_store_stubs!(@body $mode resample_trades_to_bars);
    };
    (@item $mode:tt $verb:ident) => {
        $crate::hist_store_stubs!(@body $mode $verb);
    };

    // ---- the two flavours: `Default` is `Vec::new()` for a read and `0` for a write ----------
    (@body [inert] $verb:ident) => {
        $crate::hist_store_stubs!(@sig $verb {
            ::core::result::Result::Ok(::core::default::Default::default())
        });
    };
    (@body [refuse $err:expr] $verb:ident) => {
        $crate::hist_store_stubs!(@sig $verb {
            ::core::result::Result::Err(($err)(::core::stringify!($verb)))
        });
    };

    // ---- one signature per required verb, copied from the trait -----------------------------
    (@sig load_bars $body:block) => {
        fn load_bars(
            &self,
            _: &str,
            _: &str,
            _: &str,
            _: $crate::TsRange,
        ) -> ::core::result::Result<::std::vec::Vec<::vike_model::Bar>, $crate::DataError> $body
    };
    (@sig scan_quotes $body:block) => {
        fn scan_quotes(
            &self,
            _: &str,
            _: &str,
            _: $crate::TsRange,
        ) -> ::core::result::Result<::std::vec::Vec<::vike_model::QuoteTick>, $crate::DataError>
        $body
    };
    (@sig scan_trades $body:block) => {
        fn scan_trades(
            &self,
            _: &str,
            _: &str,
            _: $crate::TsRange,
        ) -> ::core::result::Result<::std::vec::Vec<::vike_model::TradeTick>, $crate::DataError>
        $body
    };
    (@sig scan_book_updates $body:block) => {
        fn scan_book_updates(
            &self,
            _: &str,
            _: &str,
            _: $crate::TsRange,
        ) -> ::core::result::Result<::std::vec::Vec<::vike_model::BookUpdate>, $crate::DataError>
        $body
    };
    (@sig scan_symbol_properties $body:block) => {
        fn scan_symbol_properties(
            &self,
            _: &str,
            _: &str,
            _: $crate::TsRange,
        ) -> ::core::result::Result<
            ::std::vec::Vec<(i64, ::vike_model::SymbolProperties)>,
            $crate::DataError,
        > $body
    };
    (@sig scan_equity $body:block) => {
        fn scan_equity(
            &self,
            _: &str,
            _: &str,
            _: $crate::TsRange,
        ) -> ::core::result::Result<::std::vec::Vec<::vike_model::EquitySample>, $crate::DataError>
        $body
    };
    (@sig scan_exec_fills $body:block) => {
        fn scan_exec_fills(
            &self,
            _: &str,
            _: &str,
        ) -> ::core::result::Result<::std::vec::Vec<$crate::ExecFillRow>, $crate::DataError> $body
    };
    (@sig scan_exec_orders $body:block) => {
        fn scan_exec_orders(
            &self,
            _: &str,
            _: &str,
        ) -> ::core::result::Result<::std::vec::Vec<$crate::ExecOrderRow>, $crate::DataError> $body
    };
    (@sig append_bars $body:block) => {
        fn append_bars(
            &self,
            _: &str,
            _: &str,
            _: &str,
            _: &[::vike_model::Bar],
            _: ::core::option::Option<&str>,
        ) -> ::core::result::Result<usize, $crate::DataError> $body
    };
    (@sig append_quotes $body:block) => {
        fn append_quotes(
            &self,
            _: &str,
            _: &str,
            _: &[::vike_model::QuoteTick],
            _: ::core::option::Option<&str>,
        ) -> ::core::result::Result<usize, $crate::DataError> $body
    };
    (@sig append_trades $body:block) => {
        fn append_trades(
            &self,
            _: &str,
            _: &str,
            _: &[::vike_model::TradeTick],
            _: ::core::option::Option<&str>,
        ) -> ::core::result::Result<usize, $crate::DataError> $body
    };
    (@sig append_book_updates $body:block) => {
        fn append_book_updates(
            &self,
            _: &str,
            _: &str,
            _: &[::vike_model::BookUpdate],
            _: ::core::option::Option<&str>,
        ) -> ::core::result::Result<usize, $crate::DataError> $body
    };
    (@sig append_symbol_properties $body:block) => {
        fn append_symbol_properties(
            &self,
            _: &str,
            _: &str,
            _: &[(i64, ::vike_model::SymbolProperties)],
            _: ::core::option::Option<&str>,
        ) -> ::core::result::Result<usize, $crate::DataError> $body
    };
    (@sig append_equity $body:block) => {
        fn append_equity(
            &self,
            _: &str,
            _: &str,
            _: &[::vike_model::EquitySample],
            _: ::core::option::Option<&str>,
        ) -> ::core::result::Result<usize, $crate::DataError> $body
    };
    (@sig append_exec_fills $body:block) => {
        fn append_exec_fills(
            &self,
            _: &str,
            _: &str,
            _: &[$crate::ExecFillRow],
            _: ::core::option::Option<&str>,
        ) -> ::core::result::Result<usize, $crate::DataError> $body
    };
    (@sig append_exec_orders $body:block) => {
        fn append_exec_orders(
            &self,
            _: &str,
            _: &str,
            _: &[$crate::ExecOrderRow],
            _: ::core::option::Option<&str>,
        ) -> ::core::result::Result<usize, $crate::DataError> $body
    };
    (@sig resample_quotes_to_bars $body:block) => {
        fn resample_quotes_to_bars(
            &self,
            _: &str,
            _: &str,
            _: &str,
            _: $crate::TsRange,
            _: ::core::option::Option<&str>,
        ) -> ::core::result::Result<usize, $crate::DataError> $body
    };
    (@sig resample_trades_to_bars $body:block) => {
        fn resample_trades_to_bars(
            &self,
            _: &str,
            _: &str,
            _: &str,
            _: $crate::TsRange,
            _: ::core::option::Option<&str>,
        ) -> ::core::result::Result<usize, $crate::DataError> $body
    };
    (@sig $other:ident $body:block) => {
        ::core::compile_error!(::core::concat!(
            "hist_store_stubs!: `",
            ::core::stringify!($other),
            "` is not a REQUIRED HistStore verb (nor `reads`, `writes` or `all`); a defaulted verb \
             keeps the trait's own body, or is written by hand"
        ));
    };
}

#[path = "stubs_tests.rs"]
#[cfg(test)]
mod stubs_tests;
