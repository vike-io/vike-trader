use super::financial_facts;
use crate::sec_edgar::end_of_filing_day_ms;
use crate::testing::sec_edgar::{CIK, COMPANYFACTS};

#[test]
fn every_value_is_a_row_knowable_after_its_filing_day() {
    let rows = financial_facts(COMPANYFACTS.as_bytes(), CIK).expect("companyfacts");
    assert_eq!(rows.len(), 3);
    let shares = rows.iter().find(|r| r.taxonomy == "dei").expect("the dei value");
    assert_eq!(shares.unit, "shares");
    assert_eq!(shares.period_start, "", "an instant");
    assert_eq!(shares.value, 15_550_061_000.0);
    assert_eq!(shares.filed_ms, end_of_filing_day_ms(2023, 11, 3).unwrap());
    let revenues: Vec<_> = rows.iter().filter(|r| r.concept == "Revenues").collect();
    assert_eq!(revenues.len(), 2, "the original and the restatement are both kept");
    assert_eq!(revenues[0].period_start, "2022-10-01");
    assert_eq!(revenues[0].fiscal_year, "2023");
    assert_eq!(revenues[0].frame, "CY2023");
    assert_eq!(revenues[1].form, "10-K/A");
    assert_eq!(revenues[1].frame, "", "a frame is absent from a superseded value");
    assert!(revenues[1].filed_ms > revenues[0].filed_ms);
    assert!(rows.iter().all(|r| r.cik == CIK && r.entity == "EXAMPLE CORP"));
}

#[test]
fn another_companys_document_or_a_value_without_its_filing_is_refused() {
    assert!(
        financial_facts(COMPANYFACTS.as_bytes(), "0000000001")
            .unwrap_err()
            .contains("not 0000000001")
    );
    let no_filed = COMPANYFACTS.replace(r#","filed":"2024-01-10""#, "");
    assert!(financial_facts(no_filed.as_bytes(), CIK).unwrap_err().contains("filed"));
    let no_accession = COMPANYFACTS.replace(r#""accn":"0001234567-24-000002","#, "");
    assert!(financial_facts(no_accession.as_bytes(), CIK).unwrap_err().contains("accn"));
    assert!(financial_facts(b"{", CIK).unwrap_err().contains("not JSON"));
}
