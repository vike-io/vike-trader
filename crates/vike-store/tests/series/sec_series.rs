//! The two SEC table kinds — `kind=financial_fact` (reported XBRL values) and `kind=institutional_holding`
//! (Form 13F lines) — through the engine and the memory double.
//!
//! What carries weight here:
//!
//! * every column of both rows comes back as written, the empty strings included (an instant's
//!   `period_start`, an original report's `amendment_type`);
//! * the point-in-time reads never return a row knowable after the decision time, whatever period it
//!   describes;
//! * the compaction dispatch's two arms at RUNTIME (`store_kind_gate.rs` reads them as text only).

use vike_data::sec_log::SEC_VENUE;
use vike_data::{FinancialFactRow, InstitutionalHoldingRow};

const T0: i64 = 1_791_786_600_000;
const DAY: i64 = 86_400_000;
const CIK: &str = "0000320193";
const FILER: &str = "0001067983";

/// One reported value knowable at `filed_ms`; `instant` leaves `period_start` empty.
fn fact(filed_ms: i64, concept: &str, value: f64, instant: bool) -> FinancialFactRow {
    FinancialFactRow {
        filed_ms,
        cik: CIK.to_string(),
        entity: "Example Corp".to_string(),
        taxonomy: "us-gaap".to_string(),
        concept: concept.to_string(),
        unit: "USD".to_string(),
        period_start: if instant { String::new() } else { "2026-07-01".to_string() },
        period_end: "2026-09-30".to_string(),
        value,
        accession: "0000320193-26-000106".to_string(),
        fiscal_year: "2026".to_string(),
        fiscal_period: "FY".to_string(),
        form: "10-K".to_string(),
        frame: if instant { String::new() } else { "CY2026Q3".to_string() },
    }
}

/// One 13F line knowable at `filed_ms`.
fn holding(filed_ms: i64, cusip: &str, amendment_type: &str) -> InstitutionalHoldingRow {
    InstitutionalHoldingRow {
        filed_ms,
        accession: "0000950123-26-002301".to_string(),
        filer_cik: FILER.to_string(),
        filer_name: "Example Capital LLC".to_string(),
        period: "2026-09-30".to_string(),
        submission_type: if amendment_type.is_empty() { "13F-HR" } else { "13F-HR/A" }.to_string(),
        amendment_type: amendment_type.to_string(),
        issuer: "EXAMPLE CORP".to_string(),
        class_title: "COM".to_string(),
        cusip: cusip.to_string(),
        value_usd: 1_234_567_000,
        shares: 9_876_543,
        shares_type: "SH".to_string(),
        put_call: String::new(),
    }
}

mod datafusion_tests {
    use super::{CIK, DAY, FILER, T0, fact, holding};
    use vike_data::sec_log::SEC_VENUE;
    use vike_data::{HistoryStore, TimestampRange};
    use vike_store::{CompactionConfig, HistoryStoreDataFusion, MaintenanceConfig};

    #[test]
    fn both_sec_tables_roundtrip_and_read_point_in_time() {
        let dir = tempfile::tempdir().unwrap();
        let store = HistoryStoreDataFusion::open(dir.path()).unwrap();
        let facts = vec![fact(T0, "Revenues", 3.5e10, false), fact(T0, "Assets", 3.5e11, true)];
        let late = vec![fact(T0 + 2 * DAY, "Revenues", 3.6e10, false)];
        assert_eq!(store.append_financial_facts(SEC_VENUE, CIK, &facts, Some("f1")).unwrap(), 2);
        assert_eq!(store.append_financial_facts(SEC_VENUE, CIK, &late, Some("f2")).unwrap(), 1);
        assert_eq!(store.append_financial_facts(SEC_VENUE, CIK, &late, Some("f2")).unwrap(), 0);
        let all = store.scan_financial_facts(SEC_VENUE, CIK, TimestampRange::all()).unwrap();
        assert_eq!(all, [facts.clone(), late.clone()].concat(), "every column, in order");
        assert_eq!(
            store.financial_facts_as_of(SEC_VENUE, CIK, T0 + DAY, 30 * DAY).unwrap(),
            facts,
            "a restatement filed after the decision is never seen"
        );

        let lines = vec![holding(T0, "037833100", ""), holding(T0, "594918104", "")];
        let amended = vec![holding(T0 + DAY, "037833100", "RESTATEMENT")];
        store.append_institutional_holdings(SEC_VENUE, FILER, &lines, Some("h1")).unwrap();
        store.append_institutional_holdings(SEC_VENUE, FILER, &amended, Some("h2")).unwrap();
        let got =
            store.scan_institutional_holdings(SEC_VENUE, FILER, TimestampRange::all()).unwrap();
        assert_eq!(got, [lines.clone(), amended.clone()].concat());
        assert_eq!(
            store.institutional_holdings_as_of(SEC_VENUE, FILER, T0 + DAY - 1, 100 * DAY).unwrap(),
            lines
        );
        assert_eq!(
            store
                .scan_institutional_holdings_capped(
                    SEC_VENUE,
                    FILER,
                    TimestampRange::all(),
                    Some(2)
                )
                .unwrap()
                .len(),
            2,
            "the capped read keeps its complete prefix"
        );
    }

    /// The compaction dispatch's two SEC arms, at runtime. `min_parts: 1` forces the fragments through.
    #[test]
    fn run_maintenance_handles_both_sec_tables() {
        let dir = tempfile::tempdir().unwrap();
        let store = HistoryStoreDataFusion::open(dir.path()).unwrap();
        let facts = vec![fact(T0, "Revenues", 1.0, false)];
        let lines = vec![holding(T0, "037833100", "")];
        store.append_financial_facts(SEC_VENUE, CIK, &facts, Some("f")).unwrap();
        store.append_institutional_holdings(SEC_VENUE, FILER, &lines, Some("h")).unwrap();
        let config = MaintenanceConfig {
            compaction: CompactionConfig {
                target_bytes: 1 << 20,
                min_parts: 1,
                ..Default::default()
            },
            retention: None,
            retention_by_kind: Default::default(),
        };
        let report = store.run_maintenance(&config).unwrap();
        assert!(report.failed.is_empty(), "an SEC series was SKIPPED: {:?}", report.failed);
        assert_eq!(report.compaction.parts_written, 2, "both series were compacted");
        assert_eq!(
            store.scan_financial_facts(SEC_VENUE, CIK, TimestampRange::all()).unwrap(),
            facts
        );
        assert_eq!(
            store.scan_institutional_holdings(SEC_VENUE, FILER, TimestampRange::all()).unwrap(),
            lines
        );
    }
}

mod memory_tests {
    use super::{CIK, DAY, FILER, SEC_VENUE, T0, fact, holding};
    use vike_data::{HistoryStore, HistoryStoreMemory, TimestampRange};
    use vike_model::StoreKindId;

    #[test]
    fn both_sec_tables_roundtrip_memory() {
        let store = HistoryStoreMemory::new();
        let early = fact(T0, "Revenues", 1.0, false);
        let late = fact(T0 + DAY, "Revenues", 2.0, false);
        assert_eq!(store.append_financial_facts(SEC_VENUE, CIK, &[], Some("k")).unwrap(), 0);
        store
            .append_financial_facts(SEC_VENUE, CIK, &[late.clone(), early.clone()], Some("k"))
            .unwrap();
        assert_eq!(
            store.scan_financial_facts(SEC_VENUE, CIK, TimestampRange::all()).unwrap(),
            vec![early.clone(), late],
            "ascending, and an empty batch did not burn the key"
        );
        assert_eq!(store.financial_facts_as_of(SEC_VENUE, CIK, T0, DAY).unwrap(), vec![early]);
        let line = holding(T0, "037833100", "");
        store
            .append_institutional_holdings(SEC_VENUE, FILER, std::slice::from_ref(&line), Some("h"))
            .unwrap();
        assert_eq!(
            store.institutional_holdings_as_of(SEC_VENUE, FILER, T0, 0).unwrap(),
            vec![line]
        );
        let kinds: Vec<StoreKindId> =
            store.list_series().unwrap().into_iter().map(|s| s.kind).collect();
        assert!(kinds.contains(&StoreKindId::FinancialFact), "{kinds:?}");
        assert!(kinds.contains(&StoreKindId::InstitutionalHolding), "{kinds:?}");
    }
}
