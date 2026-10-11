//! Registry rows — price transforms (price.py), no params, no warm-up.
use super::*;

pub(super) fn rows() -> Vec<IndicatorMeta> {
    use Category::*;
    use RenderKind::*;
    vec![
        // ---- price transforms (price.py) — no params, no warm-up ----
        IndicatorMeta {
            name: "avgprice",
            pretty: "Average Price",
            category: Price,
            kind: Overlay,
            outputs: out! {"avgprice":Line},
            bands: &[],
            batch_only: false,
            make: mk::<Avgprice>,
            params: &[],
            make_with: |_| Box::new(Avgprice::new()),
            factory: None,
        },
        IndicatorMeta {
            name: "medprice",
            pretty: "Median Price",
            category: Price,
            kind: Overlay,
            outputs: out! {"medprice":Line},
            bands: &[],
            batch_only: false,
            make: mk::<Medprice>,
            params: &[],
            make_with: |_| Box::new(Medprice::new()),
            factory: None,
        },
        IndicatorMeta {
            name: "typprice",
            pretty: "Typical Price",
            category: Price,
            kind: Overlay,
            outputs: out! {"typprice":Line},
            bands: &[],
            batch_only: false,
            make: mk::<Typprice>,
            params: &[],
            make_with: |_| Box::new(Typprice::new()),
            factory: None,
        },
        IndicatorMeta {
            name: "wclprice",
            pretty: "Weighted Close",
            category: Price,
            kind: Overlay,
            outputs: out! {"wclprice":Line},
            bands: &[],
            batch_only: false,
            make: mk::<Wclprice>,
            params: &[],
            make_with: |_| Box::new(Wclprice::new()),
            factory: None,
        },
    ]
}
