//! Named outputs for the multi-line indicators people reach for most.
//!
//! Every indicator's `next(&bar)` returns an `f64` or an `[f64; N]`. For the nine below, `next_named`
//! returns a struct whose fields are the registry row's output names, so `bands.upper` replaces
//! `bands[0]`. It is `Indicator::on_bar` read by position — no second arithmetic — and
//! `named_output!`'s generated test holds the field names to the registry row's `outputs`, in order,
//! so the struct cannot drift from the metadata. Every other multi-line indicator keeps the array
//! (and `registry::compute` names its lines at run time); adding one here is one `named_output!` row.

use super::{Adx, Aroon, Bollinger, Donchian, Ichimoku, Keltner, Macd, Stochastic, Vortex};

/// `$Out` carries `$Ind`'s lines; `$field` names are the Rust spelling of the registry names `$reg`.
macro_rules! named_output {
    (
        $(#[$m:meta])*
        $Out:ident for $Ind:ident, $key:literal, [ $( $field:ident = $reg:literal ),+ $(,)? ]
    ) => {
        $(#[$m])*
        #[derive(Clone, Copy, Debug, PartialEq)]
        pub struct $Out {
            $(
                #[doc = concat!("The `", $reg, "` line.")]
                pub $field: f64,
            )+
        }
        impl $Ind {
            /// Feed the next bar and get this bar's lines by name. This IS
            /// [`crate::Indicator::on_bar`] read by position, bit for bit; `NaN` while warming up.
            pub fn next_named(&mut self, bar: &vike_marketdata::Bar) -> $Out {
                let mut lines = $crate::Indicator::on_bar(self, bar).into_iter();
                $Out { $( $field: lines.next().unwrap_or(f64::NAN), )+ }
            }
        }
        #[cfg(test)]
        #[test]
        #[allow(non_snake_case, clippy::allow_attributes)]
        fn $Out() {
            let row = $crate::registry::get($key).expect("the row this output belongs to");
            let names: Vec<&str> = row.outputs.iter().map(|o| o.name).collect();
            assert_eq!(names, [ $( $reg ),+ ], "{}: field order is the registry's output order", $key);
        }
    };
}

named_output! {
    /// MACD's three lines.
    MacdOutput for Macd, "macd", [macd = "macd", signal = "signal", hist = "hist"]
}
named_output! {
    /// Bollinger Bands' three lines.
    BollingerOutput for Bollinger, "bollinger", [upper = "upper", mid = "mid", lower = "lower"]
}
named_output! {
    /// Keltner Channel's three lines.
    KeltnerOutput for Keltner, "keltner", [upper = "upper", mid = "mid", lower = "lower"]
}
named_output! {
    /// Donchian Channel's three lines.
    DonchianOutput for Donchian, "donchian", [upper = "upper", mid = "mid", lower = "lower"]
}
named_output! {
    /// Stochastic's two lines (`k` is `%K`, `d` is `%D`).
    StochasticOutput for Stochastic, "stochastic", [k = "%K", d = "%D"]
}
named_output! {
    /// ADX and its two directional-movement lines.
    AdxOutput for Adx, "adx", [adx = "adx", plus_di = "plus_di", minus_di = "minus_di"]
}
named_output! {
    /// Aroon's two lines.
    AroonOutput for Aroon, "aroon", [aroon_up = "aroon_up", aroon_down = "aroon_down"]
}
named_output! {
    /// Vortex's two lines.
    VortexOutput for Vortex, "vortex", [vi_plus = "vi_plus", vi_minus = "vi_minus"]
}
named_output! {
    /// Ichimoku's five lines. `chikou` reads FUTURE bars, so `next_named` reports a causal best
    /// effort for it; `registry::compute` (the batch path) is authoritative.
    IchimokuOutput for Ichimoku, "ichimoku", [
        tenkan = "tenkan", kijun = "kijun", senkou_a = "senkou_a", senkou_b = "senkou_b",
        chikou = "chikou",
    ]
}
