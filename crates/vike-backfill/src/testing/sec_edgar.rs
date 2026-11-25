//! Hand-built SEC fixtures, written to the SEC's documented formats (the EDGAR complete submission
//! text, the submissions JSON, `companyfacts`, the Form 13F data-set tables). The company, the manager
//! and every number are invented: `EXAMPLE CORP` (CIK [`CIK`]) and `EXAMPLE CAPITAL` ([`FILER`]).

use std::io::Write;

use flate2::Compression;
use flate2::write::DeflateEncoder;

/// The example company's CIK.
pub const CIK: &str = "0001234567";

/// The example manager's CIK.
pub const FILER: &str = "0009876543";

/// A 10-K accepted 2023-11-02 18:08:27 New York daylight time (22:08:27 UTC, the UTC day 2023-11-02),
/// dated 2023-11-03: an inline XBRL primary document with a table of contents ending at `Item 16.` and a
/// forward-looking-statements preamble after it (the shape of Apple's 10-K of 2024-11-01, read live), a
/// trading symbol, and two documents the text history skips (an exhibit 21 and a uuencoded graphic).
pub const TEN_K: &str = "<SEC-DOCUMENT>0001234567-23-000106.txt : 20231103
<SEC-HEADER>0001234567-23-000106.hdr.sgml : 20231103
<ACCEPTANCE-DATETIME>20231102180827
ACCESSION NUMBER:\t\t0001234567-23-000106
CONFORMED SUBMISSION TYPE:\t10-K
PUBLIC DOCUMENT COUNT:\t\t3
CONFORMED PERIOD OF REPORT:\t20230930
FILED AS OF DATE:\t\t20231103
DATE AS OF CHANGE:\t\t20231102

FILER:

\tCOMPANY DATA:\t
\t\tCOMPANY CONFORMED NAME:\t\t\tEXAMPLE CORP
\t\tCENTRAL INDEX KEY:\t\t\t0001234567
\t\tSTANDARD INDUSTRIAL CLASSIFICATION:\tELECTRONIC COMPUTERS [3571]
\t\tFISCAL YEAR END:\t\t\t0930

\tFILING VALUES:
\t\tFORM TYPE:\t\t10-K
\t\tSEC ACT:\t\t1934 Act
</SEC-HEADER>
<DOCUMENT>
<TYPE>10-K
<SEQUENCE>1
<FILENAME>exmp-20230930.htm
<DESCRIPTION>10-K
<TEXT>
<XBRL>
<?xml version='1.0' encoding='ASCII'?>
<html xmlns:ix=\"http://www.xbrl.org/2013/inlineXBRL\"><head><title>exmp-20230930</title></head><body>
<div style=\"display:none\"><ix:header><ix:hidden><ix:nonNumeric name=\"dei:AmendmentFlag\" contextRef=\"c-1\">false</ix:nonNumeric></ix:hidden></ix:header></div>
<div><span>Trading Symbol</span><span><ix:nonNumeric name=\"dei:TradingSymbol\" contextRef=\"c-1\">EXMP</ix:nonNumeric></span></div>
<div>TABLE OF CONTENTS</div>
<table><tr><td>Item 1.</td><td>Business</td><td>1</td></tr>
<tr><td>Item 1A.</td><td>Risk Factors</td><td>5</td></tr>
<tr><td>Item 7.</td><td>Management&#8217;s Discussion and Analysis</td><td>20</td></tr>
<tr><td>Item 16.</td><td>Form 10-K Summary</td><td>56</td></tr></table>
<div>This Annual Report on Form 10-K contains forward-looking statements, within the meaning of the Private Securities Litigation Reform Act of 1995, that involve risks and uncertainties. Many of the forward-looking statements are located in Part II, Item 7 of this Form 10-K.</div>
<div><span style=\"font-weight:700\">Item 1.&#160;&#160;&#160;&#160;Business</span></div>
<div>Example Corp designs and sells example products to example customers worldwide.</div>
<div><span style=\"font-weight:700\">Item 1A.&#160;&#160;&#160;&#160;Risk Factors</span></div>
<div>The Company&#8217;s business can be affected by many risks, described below in detail.</div>
<div><span style=\"font-weight:700\">Item 7.&#160;&#160;&#160;&#160;Management&#8217;s Discussion and Analysis</span></div>
<div>Net sales increased 5% compared with the prior year, driven by higher demand in every segment.</div>
<div><span style=\"font-weight:700\">Item 16.&#160;&#160;&#160;&#160;Form 10-K Summary</span></div>
<div>None.</div>
</body></html>
</XBRL>
</TEXT>
</DOCUMENT>
<DOCUMENT>
<TYPE>EX-21.1
<SEQUENCE>2
<FILENAME>exmp-ex211.htm
<DESCRIPTION>SUBSIDIARIES
<TEXT>
<html><body>Subsidiaries</body></html>
</TEXT>
</DOCUMENT>
<DOCUMENT>
<TYPE>GRAPHIC
<SEQUENCE>3
<FILENAME>logo.jpg
<TEXT>
begin 644 logo.jpg
M_]C_X``02D9)1@`!`0$`8`!@``#_VP!#``@&!@<&!0@'!P<)\"0@*#!0-#`L+
end
</TEXT>
</DOCUMENT>
</SEC-DOCUMENT>
";

/// An 8-K accepted 2024-02-01 16:30:12 New York standard time (21:30:12 UTC): Items 2.02 and 9.01 and
/// the press release as exhibit 99.1, which describes itself by its type and carries a head titled
/// `Document`, as Apple's of 2024-10-31 does (read live).
pub const EIGHT_K: &str = "<SEC-DOCUMENT>0001234567-24-000010.txt : 20240201
<SEC-HEADER>0001234567-24-000010.hdr.sgml : 20240201
<ACCEPTANCE-DATETIME>20240201163012
ACCESSION NUMBER:\t\t0001234567-24-000010
CONFORMED SUBMISSION TYPE:\t8-K
PUBLIC DOCUMENT COUNT:\t\t2
CONFORMED PERIOD OF REPORT:\t20240201
ITEM INFORMATION:\t\tResults of Operations and Financial Condition
ITEM INFORMATION:\t\tFinancial Statements and Exhibits
FILED AS OF DATE:\t\t20240201
DATE AS OF CHANGE:\t\t20240201

FILER:

\tCOMPANY DATA:\t
\t\tCOMPANY CONFORMED NAME:\t\t\tEXAMPLE CORP
\t\tCENTRAL INDEX KEY:\t\t\t0001234567
</SEC-HEADER>
<DOCUMENT>
<TYPE>8-K
<SEQUENCE>1
<FILENAME>exmp-20240201.htm
<DESCRIPTION>8-K
<TEXT>
<html><body><p>Item 2.02 Results of Operations and Financial Condition.</p>
<p>On February 1, 2024, Example Corp announced its results for the quarter.</p>
<p>Item 9.01 Financial Statements and Exhibits.</p><p>(d) Exhibit 99.1 Press release.</p></body></html>
</TEXT>
</DOCUMENT>
<DOCUMENT>
<TYPE>EX-99.1
<SEQUENCE>2
<FILENAME>exmp-ex991.htm
<DESCRIPTION>EX-99.1
<TEXT>
<html><head><title>Document</title></head><body><p>Example Corp Reports First Quarter Results</p><p>Revenue of $1.0 billion.</p></body></html>
</TEXT>
</DOCUMENT>
</SEC-DOCUMENT>
";

/// A 10-K/A accepted 2024-01-10 09:15:00 New York standard time, correcting [`TEN_K`] (same period).
pub const TEN_K_AMENDMENT: &str = "<SEC-DOCUMENT>0001234567-24-000002.txt : 20240110
<SEC-HEADER>0001234567-24-000002.hdr.sgml : 20240110
<ACCEPTANCE-DATETIME>20240110091500
ACCESSION NUMBER:\t\t0001234567-24-000002
CONFORMED SUBMISSION TYPE:\t10-K/A
PUBLIC DOCUMENT COUNT:\t\t1
CONFORMED PERIOD OF REPORT:\t20230930
FILED AS OF DATE:\t\t20240110

FILER:

\tCOMPANY DATA:\t
\t\tCOMPANY CONFORMED NAME:\t\t\tEXAMPLE CORP
\t\tCENTRAL INDEX KEY:\t\t\t0001234567
</SEC-HEADER>
<DOCUMENT>
<TYPE>10-K/A
<SEQUENCE>1
<FILENAME>exmp-20230930a.htm
<TEXT>
<html><body><p>Item 1A. Risk Factors</p><p>The Company restates one risk factor.</p></body></html>
</TEXT>
</DOCUMENT>
</SEC-DOCUMENT>
";

/// A Schedule 13G ABOUT the example company, filed by the example manager the same day as [`EIGHT_K`].
pub const SCHEDULE_13G: &str = "<SEC-DOCUMENT>0009876543-24-000001.txt : 20240201
<SEC-HEADER>0009876543-24-000001.hdr.sgml : 20240201
<ACCEPTANCE-DATETIME>20240201101500
ACCESSION NUMBER:\t\t0009876543-24-000001
CONFORMED SUBMISSION TYPE:\tSC 13G
PUBLIC DOCUMENT COUNT:\t\t1
FILED AS OF DATE:\t\t20240201

SUBJECT COMPANY:\t

\tCOMPANY DATA:\t
\t\tCOMPANY CONFORMED NAME:\t\t\tEXAMPLE CORP
\t\tCENTRAL INDEX KEY:\t\t\t0001234567

FILED BY:\t\t

\tCOMPANY DATA:\t
\t\tCOMPANY CONFORMED NAME:\t\t\tEXAMPLE CAPITAL LLC
\t\tCENTRAL INDEX KEY:\t\t\t0009876543
</SEC-HEADER>
<DOCUMENT>
<TYPE>SC 13G
<SEQUENCE>1
<FILENAME>exmp13g.txt
<TEXT>
SCHEDULE 13G
Example Capital LLC beneficially owns 6.1% of the common stock of Example Corp.
</TEXT>
</DOCUMENT>
</SEC-DOCUMENT>
";

/// The example company's submissions index: the four filings above, newest first, each
/// `acceptanceDateTime` in UTC as the SEC writes it (the same instants their headers state in New York
/// time), a Form 4, and one older page.
pub const SUBMISSIONS: &str = r#"{"cik":"1234567","entityType":"operating","name":"EXAMPLE CORP",
"tickers":["EXMP"],"exchanges":["Nasdaq"],"filings":{"recent":{
"accessionNumber":["0001234567-24-000010","0009876543-24-000001","0001234567-24-000002","0001234567-23-000106","0001234567-23-000099"],
"filingDate":["2024-02-01","2024-02-01","2024-01-10","2023-11-03","2023-11-02"],
"reportDate":["2024-02-01","","2023-09-30","2023-09-30",""],
"acceptanceDateTime":["2024-02-01T21:30:12.000Z","2024-02-01T15:15:00.000Z","2024-01-10T14:15:00.000Z","2023-11-02T22:08:27.000Z","2023-11-02T21:00:00.000Z"],
"form":["8-K","SC 13G","10-K/A","10-K","4"],
"size":[25000,4000,9000,9876543,3000],
"primaryDocument":["exmp-20240201.htm","exmp13g.txt","exmp-20230930a.htm","exmp-20230930.htm","xslF345X05/form4.xml"]},
"files":[{"name":"CIK0001234567-submissions-001.json","filingCount":2,"filingFrom":"2001-01-02","filingTo":"2015-06-30"},
{"name":"../../etc/passwd","filingCount":1,"filingFrom":"2001-01-02","filingTo":"2001-01-02"}]}}"#;

/// The example company's `companyfacts`: two durations and an instant from the 10-K (filed 2023-11-03)
/// and a restated revenue from the 10-K/A (filed 2024-01-10).
pub const COMPANYFACTS: &str = r#"{"cik":1234567,"entityName":"EXAMPLE CORP","facts":{
"dei":{"EntityCommonStockSharesOutstanding":{"label":"Entity Common Stock, Shares Outstanding","units":{"shares":[
{"end":"2023-10-20","val":15550061000,"accn":"0001234567-23-000106","fy":2023,"fp":"FY","form":"10-K","filed":"2023-11-03","frame":"CY2023Q3I"}]}}},
"us-gaap":{"Revenues":{"label":"Revenues","units":{"USD":[
{"start":"2022-10-01","end":"2023-09-30","val":383285000000,"accn":"0001234567-23-000106","fy":2023,"fp":"FY","form":"10-K","filed":"2023-11-03","frame":"CY2023"},
{"start":"2022-10-01","end":"2023-09-30","val":383300000000,"accn":"0001234567-24-000002","fy":2023,"fp":"FY","form":"10-K/A","filed":"2024-01-10"}]}}}}}"#;

/// The 13F data set's `SUBMISSION.tsv`: a report filed 2024-02-14, its amendment filed 2024-02-20 and a
/// notice; an older report filed 2022-11-14, whose values are in thousands.
pub const FORM13F_SUBMISSION: &str =
    "ACCESSION_NUMBER\tFILING_DATE\tSUBMISSIONTYPE\tCIK\tPERIODOFREPORT
0009876543-24-000002\t14-FEB-2024\t13F-HR\t9876543\t31-DEC-2023
0009876543-24-000003\t20-FEB-2024\t13F-HR/A\t9876543\t31-DEC-2023
0001111111-24-000001\t14-FEB-2024\t13F-NT\t1111111\t31-DEC-2023
0009876543-22-000009\t14-NOV-2022\t13F-HR\t9876543\t30-SEP-2022
";

/// The data set's `COVERPAGE.tsv` (a column the reader does not use, between the ones it does).
pub const FORM13F_COVERPAGE: &str = "ACCESSION_NUMBER\tREPORTCALENDARORQUARTER\tISAMENDMENT\tAMENDMENTNO\tAMENDMENTTYPE\tFILINGMANAGER_NAME
0009876543-24-000002\t31-DEC-2023\tN\t\t\tEXAMPLE CAPITAL LLC
0009876543-24-000003\t31-DEC-2023\tY\t1\tRESTATEMENT\tEXAMPLE CAPITAL LLC
0001111111-24-000001\t31-DEC-2023\tN\t\t\tOTHER NOTICE FILER
0009876543-22-000009\t30-SEP-2022\tN\t\t\tEXAMPLE CAPITAL LLC
";

/// The data set's `INFOTABLE.tsv`.
pub const FORM13F_INFOTABLE: &str = "ACCESSION_NUMBER\tINFOTABLE_SK\tNAMEOFISSUER\tTITLEOFCLASS\tCUSIP\tFIGI\tVALUE\tSSHPRNAMT\tSSHPRNAMTTYPE\tPUTCALL\tINVESTMENTDISCRETION
0009876543-24-000002\t1\tEXAMPLE CORP\tCOM\t123456789\t\t1500000000\t10000000\tSH\t\tSOLE
0009876543-24-000002\t2\tOTHER CO\tCL A\t987654321\t\t250000\t5000\tSH\tPut\tSOLE
0009876543-24-000003\t3\tEXAMPLE CORP\tCOM\t123456789\t\t1600000000\t10500000\tSH\t\tSOLE
0009876543-22-000009\t4\tEXAMPLE CORP\tCOM\t123456789\t\t1200000\t9000000\tSH\t\tSOLE
";

/// The three tables as a data-set zip.
pub fn form13f_zip() -> Vec<u8> {
    zip_of_members(&[
        ("SUBMISSION.tsv", FORM13F_SUBMISSION.as_bytes()),
        ("COVERPAGE.tsv", FORM13F_COVERPAGE.as_bytes()),
        ("INFOTABLE.tsv", FORM13F_INFOTABLE.as_bytes()),
    ])
}

/// A zip of `members`, each deflated, with a central directory a reader can trust.
pub fn zip_of_members(members: &[(&str, &[u8])]) -> Vec<u8> {
    let mut out = Vec::new();
    let mut central = Vec::new();
    for (name, body) in members {
        let mut deflate = DeflateEncoder::new(Vec::new(), Compression::default());
        deflate.write_all(body).expect("deflate into memory");
        let stream = deflate.finish().expect("deflate into memory");
        let mut crc = flate2::Crc::new();
        crc.update(body);
        let offset = u32::try_from(out.len()).expect("a small zip");
        let sizes = [crc.sum(), len32(&stream), len32(body)];
        let name_len = u16::try_from(name.len()).expect("a short name").to_le_bytes();
        out.extend(b"PK\x03\x04\x14\x00\x00\x00\x08\x00\x00\x00\x00\x00");
        sizes.iter().for_each(|v| out.extend(v.to_le_bytes()));
        out.extend(name_len);
        out.extend([0u8; 2]);
        out.extend(name.as_bytes());
        out.extend(&stream);
        central.extend(b"PK\x01\x02\x14\x00\x14\x00\x00\x00\x08\x00\x00\x00\x00\x00");
        sizes.iter().for_each(|v| central.extend(v.to_le_bytes()));
        central.extend(name_len);
        central.extend([0u8; 12]);
        central.extend(offset.to_le_bytes());
        central.extend(name.as_bytes());
    }
    let (at, size) = (len32(&out), len32(&central));
    let count = u16::try_from(members.len()).expect("a few members").to_le_bytes();
    out.extend(&central);
    out.extend(b"PK\x05\x06\x00\x00\x00\x00");
    out.extend(count);
    out.extend(count);
    out.extend(size.to_le_bytes());
    out.extend(at.to_le_bytes());
    out.extend([0u8; 2]);
    out
}

fn len32(bytes: &[u8]) -> u32 {
    u32::try_from(bytes.len()).expect("a small zip")
}
