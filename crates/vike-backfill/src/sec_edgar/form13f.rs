//! **The SEC's Form 13F data sets**: one zip a quarter (`2024q1_form13f.zip`), tab-separated tables
//! with a header row, joined on `ACCESSION_NUMBER`. Three are read:
//!
//! - `SUBMISSION.tsv`: `ACCESSION_NUMBER`, `FILING_DATE`, `SUBMISSIONTYPE`, `CIK`, `PERIODOFREPORT`;
//! - `COVERPAGE.tsv`: `ACCESSION_NUMBER`, `ISAMENDMENT`, `AMENDMENTTYPE`, `FILINGMANAGER_NAME`;
//! - `INFOTABLE.tsv`: `ACCESSION_NUMBER`, `NAMEOFISSUER`, `TITLEOFCLASS`, `CUSIP`, `VALUE`, `SSHPRNAMT`,
//!   `SSHPRNAMTTYPE`, `PUTCALL`.
//!
//! Columns are found by their header name, so a column the SEC adds or moves is harmless and a
//! missing one refuses the file. Dates are the data sets' `DD-MON-YYYY` (`05-JAN-2024`).
//!
//! - **Knowable**: a filing states its DATE only, so each line is knowable at the end of that EDGAR day
//!   (`super::end_of_filing_day_ms`): the UTC day AFTER the filing date. A day of this format is that UTC
//!   day.
//! - **Value**: whole dollars. A report filed before [`DOLLARS_SINCE`] stated thousands (the 13F rule
//!   change of 2023), and its value is multiplied out.
//! - A filing with no information table (`13F-NT`, a notice) gives no line.
//!
//! The information table runs to millions of lines a quarter, so it is STREAMED and only the lines of
//! the filings asked for are kept ([`holdings_of`]).

use std::collections::HashMap;
use std::io::{BufRead, BufReader};

use vike_data::InstitutionalHoldingRow;
use vike_model::MS_PER_DAY;

use super::zip_members::{find_member, member_reader, zip_members};

/// The first filing date whose `VALUE` is in dollars rather than thousands.
pub const DOLLARS_SINCE: (i64, u32, u32) = (2023, 1, 3);

/// The amendment flag's "yes".
const YES: &str = "Y";

/// One 13F filing of a data set, from its submission and cover-page rows.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Form13fFiling {
    /// `0001067983-24-000006`.
    pub accession: String,
    /// The manager's CIK, ten digits.
    pub filer_cik: String,
    /// The manager's name from the cover page.
    pub filer_name: String,
    /// `13F-HR`, `13F-HR/A`, `13F-NT`.
    pub submission_type: String,
    /// The quarter's end, `YYYY-MM-DD`.
    pub period: String,
    /// `RESTATEMENT` or `NEW HOLDINGS` for an amendment; empty otherwise.
    pub amendment_type: String,
    /// When its lines became knowable, epoch milliseconds UTC.
    pub filed_ms: i64,
    /// Whether its `VALUE` is in thousands (filed before [`DOLLARS_SINCE`]).
    pub value_in_thousands: bool,
}

impl Form13fFiling {
    /// The UTC day (its midnight, epoch milliseconds) its lines are stored under.
    pub fn day(&self) -> i64 {
        self.filed_ms.div_euclid(MS_PER_DAY) * MS_PER_DAY
    }
}

/// Every filing of the data set `zip`, from `SUBMISSION.tsv` and `COVERPAGE.tsv`.
pub fn read_filings(zip: &[u8]) -> Result<Vec<Form13fFiling>, String> {
    let members = zip_members(zip)?;
    let mut covers: HashMap<String, (String, String)> = HashMap::new();
    for_each_row(
        zip,
        find_member(&members, "COVERPAGE.tsv")?,
        &["ACCESSION_NUMBER", "ISAMENDMENT", "AMENDMENTTYPE", "FILINGMANAGER_NAME"],
        |f| {
            let amendment = if f[1] == YES { f[2].to_string() } else { String::new() };
            covers.insert(f[0].to_string(), (f[3].to_string(), amendment));
            Ok(())
        },
    )?;
    let mut out = Vec::new();
    let columns = ["ACCESSION_NUMBER", "FILING_DATE", "SUBMISSIONTYPE", "CIK", "PERIODOFREPORT"];
    for_each_row(zip, find_member(&members, "SUBMISSION.tsv")?, &columns, |f| {
        let filed =
            data_set_date(f[1]).ok_or_else(|| format!("FILING_DATE {:?} is not a date", f[1]))?;
        let period = data_set_date(f[4])
            .ok_or_else(|| format!("PERIODOFREPORT {:?} is not a date", f[4]))?;
        let cik = f[3].trim_start_matches('0');
        if cik.is_empty() || cik.len() > 10 || !cik.bytes().all(|b| b.is_ascii_digit()) {
            return Err(format!("CIK {:?} is not one", f[3]));
        }
        let (name, amendment) = covers.get(f[0]).cloned().unwrap_or_default();
        out.push(Form13fFiling {
            accession: f[0].to_string(),
            filer_cik: format!("{cik:0>10}"),
            filer_name: name,
            submission_type: f[2].to_string(),
            period: super::date_label(period),
            amendment_type: amendment,
            filed_ms: super::end_of_filing_day_ms(filed.0, filed.1, filed.2).ok_or("a date")?,
            value_in_thousands: filed < DOLLARS_SINCE,
        });
        Ok(())
    })?;
    Ok(out)
}

/// The information-table lines of `filings`, streamed out of `INFOTABLE.tsv`, in the table's order.
pub fn holdings_of(
    zip: &[u8],
    filings: &[&Form13fFiling],
) -> Result<Vec<InstitutionalHoldingRow>, String> {
    let wanted: HashMap<&str, &Form13fFiling> =
        filings.iter().map(|f| (f.accession.as_str(), *f)).collect();
    let members = zip_members(zip)?;
    let columns = [
        "ACCESSION_NUMBER",
        "NAMEOFISSUER",
        "TITLEOFCLASS",
        "CUSIP",
        "VALUE",
        "SSHPRNAMT",
        "SSHPRNAMTTYPE",
        "PUTCALL",
    ];
    let mut out = Vec::new();
    for_each_row(zip, find_member(&members, "INFOTABLE.tsv")?, &columns, |f| {
        let Some(filing) = wanted.get(f[0]) else { return Ok(()) };
        let value = whole(f[4]).ok_or_else(|| format!("VALUE {:?} is not a number", f[4]))?;
        let shares = whole(f[5]).ok_or_else(|| format!("SSHPRNAMT {:?} is not a number", f[5]))?;
        out.push(InstitutionalHoldingRow {
            filed_ms: filing.filed_ms,
            accession: filing.accession.clone(),
            filer_cik: filing.filer_cik.clone(),
            filer_name: filing.filer_name.clone(),
            period: filing.period.clone(),
            submission_type: filing.submission_type.clone(),
            amendment_type: filing.amendment_type.clone(),
            issuer: f[1].to_string(),
            class_title: f[2].to_string(),
            cusip: f[3].to_string(),
            value_usd: if filing.value_in_thousands { value.saturating_mul(1000) } else { value },
            shares,
            shares_type: f[6].to_string(),
            put_call: f[7].to_string(),
        });
        Ok(())
    })?;
    Ok(out)
}

/// Call `row` with the `columns` of every data row of the tab-separated `member`, in that order. A
/// missing column refuses the table; a row's own refusal names its line.
fn for_each_row(
    zip: &[u8],
    member: &super::zip_members::ZipMember,
    columns: &[&str],
    mut row: impl FnMut(&[&str]) -> Result<(), String>,
) -> Result<(), String> {
    let mut lines = BufReader::new(member_reader(zip, member)?);
    let mut line = String::new();
    let table = &member.name;
    lines.read_line(&mut line).map_err(|e| format!("{table}: {e}"))?;
    let header: Vec<String> = fields(&line).map(str::to_ascii_uppercase).collect();
    let at: Vec<usize> = columns
        .iter()
        .map(|c| {
            header.iter().position(|h| h == c).ok_or_else(|| format!("{table} has no {c} column"))
        })
        .collect::<Result<_, _>>()?;
    let mut number = 1;
    loop {
        line.clear();
        number += 1;
        if lines.read_line(&mut line).map_err(|e| format!("{table}: {e}"))? == 0 {
            return Ok(());
        }
        if line.trim().is_empty() {
            continue;
        }
        let all: Vec<&str> = fields(&line).collect();
        let picked: Vec<&str> = at.iter().map(|&i| all.get(i).copied().unwrap_or("")).collect();
        row(&picked).map_err(|why| format!("{table} line {number}: {why}"))?;
    }
}

/// A line's tab-separated fields, trimmed and unquoted.
fn fields(line: &str) -> impl Iterator<Item = &str> {
    line.trim_end_matches(['\r', '\n']).split('\t').map(|f| f.trim().trim_matches('"'))
}

/// A data set's `DD-MON-YYYY` (or `YYYY-MM-DD`) date.
fn data_set_date(text: &str) -> Option<(i64, u32, u32)> {
    const MONTHS: [&str; 12] =
        ["JAN", "FEB", "MAR", "APR", "MAY", "JUN", "JUL", "AUG", "SEP", "OCT", "NOV", "DEC"];
    let parts: Vec<&str> = text.split('-').collect();
    if let [day, month, year] = parts[..]
        && let Some(m) = MONTHS.iter().position(|n| n.eq_ignore_ascii_case(month))
    {
        let (year, day) = (year.parse().ok()?, day.parse().ok()?);
        let month = u32::try_from(m + 1).ok()?;
        return vike_utility::calendar::is_real_day(year, month, day).then_some((year, month, day));
    }
    super::parse_date(text)
}

/// A whole number: digits, or a decimal that rounds to one.
fn whole(text: &str) -> Option<i64> {
    text.parse::<i64>().ok().or_else(|| {
        let value = text.parse::<f64>().ok()?;
        // shortcut: a fractional VALUE is rounded to the dollar; none was seen in the SEC's layout.
        value.is_finite().then(|| value.round() as i64)
    })
}

#[cfg(test)]
#[path = "form13f_tests.rs"]
mod form13f_tests;
