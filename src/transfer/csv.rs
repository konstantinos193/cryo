//! CSV, the way the other browsers actually write it (RFC 4180).
//!
//! Password and history exports are CSV everywhere, and every exporter
//! has its own quirks: a UTF-8 BOM from Excel, CRLF or LF, quoted fields
//! holding commas and newlines, doubled quotes inside them. One reader
//! and one writer here mean the quirks are handled in a single place and
//! the column-mapping modules stay about columns.
//!
//! Pure text in, pure text out: nothing here touches the disk or the
//! domain, which is what makes it cheap to test.

/// A parsed document: the header row, then the data rows.
#[derive(Debug, PartialEq, Eq)]
pub struct Table {
    header: Vec<String>,
    rows: Vec<Vec<String>>,
}

impl Table {
    /// Parse a whole document. The first non-empty row is the header.
    /// A row with fewer fields than the header is padded, a longer one
    /// is kept whole — an exporter that writes a stray comma should not
    /// cost the user the row.
    pub fn parse(text: &str) -> Self {
        let mut rows = parse_rows(text);
        let header = if rows.is_empty() {
            Vec::new()
        } else {
            rows.remove(0)
        };
        Self { header, rows }
    }

    pub fn header(&self) -> &[String] {
        &self.header
    }

    pub fn rows(&self) -> &[Vec<String>] {
        &self.rows
    }

    /// Index of the first header whose normalized name is in `names`.
    /// Names arrive lowercase; the header is normalized the same way, so
    /// `"Login Name"`, `"login name"` and `"\"Login Name\""` all match.
    pub fn column(&self, names: &[&str]) -> Option<usize> {
        names.iter().find_map(|wanted| {
            self.header
                .iter()
                .position(|h| normalize_header(h) == *wanted)
        })
    }

    /// One field of one row, trimmed. Missing column or short row reads
    /// as empty — the column mapping decides whether that is fatal.
    pub fn field(&self, row: usize, column: Option<usize>) -> &str {
        match (column, self.rows.get(row)) {
            (Some(c), Some(r)) => r.get(c).map(|s| s.trim()).unwrap_or(""),
            _ => "",
        }
    }
}

/// Header names compare without case, surrounding space, or the
/// underscores and spaces exporters disagree about (`login_uri` vs
/// `login uri`).
fn normalize_header(raw: &str) -> String {
    raw.trim()
        .trim_matches('"')
        .trim()
        .to_lowercase()
        .replace('_', " ")
}

/// Split a document into rows of fields.
fn parse_rows(text: &str) -> Vec<Vec<String>> {
    // Excel and some exporters lead with a UTF-8 BOM.
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);
    let mut rows = Vec::new();
    let mut row: Vec<String> = Vec::new();
    let mut field = String::new();
    let mut quoted = false;
    let mut chars = text.chars().peekable();
    // True once anything of the current row exists — an empty trailing
    // line must not become a row of one empty field.
    let mut started = false;

    while let Some(c) = chars.next() {
        if quoted {
            match c {
                '"' => {
                    // A doubled quote is a literal one; a single quote
                    // ends the quoted run.
                    if chars.peek() == Some(&'"') {
                        chars.next();
                        field.push('"');
                    } else {
                        quoted = false;
                    }
                }
                _ => field.push(c),
            }
            continue;
        }
        match c {
            '"' => {
                quoted = true;
                started = true;
            }
            ',' => {
                row.push(std::mem::take(&mut field));
                started = true;
            }
            '\r' => {
                // CRLF and a lone CR both end the row.
                if chars.peek() == Some(&'\n') {
                    chars.next();
                }
                end_row(&mut rows, &mut row, &mut field, &mut started);
            }
            '\n' => end_row(&mut rows, &mut row, &mut field, &mut started),
            _ => {
                field.push(c);
                started = true;
            }
        }
    }
    // An unterminated quote at EOF keeps what it had rather than
    // dropping the last row.
    if started || !field.is_empty() || !row.is_empty() {
        row.push(field);
        rows.push(row);
    }
    rows
}

fn end_row(
    rows: &mut Vec<Vec<String>>,
    row: &mut Vec<String>,
    field: &mut String,
    started: &mut bool,
) {
    if *started || !row.is_empty() {
        row.push(std::mem::take(field));
        rows.push(std::mem::take(row));
    }
    *started = false;
}

/// Build a document other programs can read back.
#[derive(Default)]
pub struct Writer {
    out: String,
}

impl Writer {
    pub fn new(header: &[&str]) -> Self {
        let mut w = Self::default();
        w.row(header.iter().copied());
        w
    }

    /// Append one row. CRLF line endings: Excel and the Chromium
    /// importers both accept them, and this is a Windows program.
    pub fn row<'a>(&mut self, fields: impl Iterator<Item = &'a str>) {
        let mut first = true;
        for field in fields {
            if !first {
                self.out.push(',');
            }
            first = false;
            self.out.push_str(&quote(field));
        }
        self.out.push_str("\r\n");
    }

    pub fn finish(self) -> String {
        self.out
    }
}

/// Quote a field only where it needs it, and double the quotes inside.
fn quote(field: &str) -> String {
    let needs = field.chars().any(|c| matches!(c, ',' | '"' | '\n' | '\r'))
        || field.starts_with(' ')
        || field.ends_with(' ');
    if !needs {
        return field.to_string();
    }
    let mut out = String::with_capacity(field.len() + 2);
    out.push('"');
    for c in field.chars() {
        if c == '"' {
            out.push('"');
        }
        out.push(c);
    }
    out.push('"');
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plain_rows_split_on_commas() {
        let t = Table::parse("a,b\n1,2\n3,4\n");
        assert_eq!(t.header(), ["a", "b"]);
        assert_eq!(t.rows().len(), 2);
        assert_eq!(t.rows()[1], ["3", "4"]);
    }

    #[test]
    fn crlf_lone_cr_and_lf_all_end_rows() {
        assert_eq!(Table::parse("a\r\n1\r\n").rows().len(), 1);
        assert_eq!(Table::parse("a\r1\r").rows().len(), 1);
        assert_eq!(Table::parse("a\n1").rows().len(), 1);
    }

    #[test]
    fn a_bom_is_not_part_of_the_first_header() {
        let t = Table::parse("\u{feff}url,password\nhttps://a,pw\n");
        assert_eq!(t.header()[0], "url");
        assert!(t.column(&["url"]).is_some());
    }

    #[test]
    fn quoted_fields_keep_commas_quotes_and_newlines() {
        let t = Table::parse("a,b\n\"x,y\",\"he said \"\"hi\"\"\"\n");
        assert_eq!(t.rows()[0][0], "x,y");
        assert_eq!(t.rows()[0][1], "he said \"hi\"");
        let t = Table::parse("a\n\"line1\nline2\"\n");
        assert_eq!(t.rows()[0][0], "line1\nline2");
        assert_eq!(t.rows().len(), 1);
    }

    #[test]
    fn empty_trailing_lines_are_not_rows() {
        let t = Table::parse("a,b\n1,2\n\n\n");
        assert_eq!(t.rows().len(), 1);
        assert!(Table::parse("").rows().is_empty());
        assert!(Table::parse("").header().is_empty());
    }

    #[test]
    fn empty_fields_survive() {
        let t = Table::parse("a,b,c\n1,,3\n");
        assert_eq!(t.rows()[0], ["1", "", "3"]);
    }

    #[test]
    fn an_unterminated_quote_keeps_what_it_had() {
        let t = Table::parse("a,b\n\"unterminated,2\n");
        assert_eq!(t.rows().len(), 1);
        assert_eq!(t.rows()[0][0], "unterminated,2\n");
    }

    #[test]
    fn column_lookup_ignores_case_quotes_and_underscores() {
        let t = Table::parse("\"Login URI\",Login_Username,PASSWORD\nu,n,p\n");
        assert_eq!(t.column(&["login uri"]), Some(0));
        assert_eq!(t.column(&["login username"]), Some(1));
        assert_eq!(t.column(&["password"]), Some(2));
        assert_eq!(t.column(&["nope"]), None);
    }

    #[test]
    fn column_lookup_prefers_the_earlier_name_in_the_list() {
        let t = Table::parse("name,url\nn,u\n");
        assert_eq!(t.column(&["url", "name"]), Some(1));
    }

    #[test]
    fn field_reads_are_forgiving_about_short_rows() {
        let t = Table::parse("a,b,c\n1\n");
        assert_eq!(t.field(0, Some(0)), "1");
        assert_eq!(t.field(0, Some(2)), "");
        assert_eq!(t.field(0, None), "");
        assert_eq!(t.field(9, Some(0)), "");
    }

    #[test]
    fn writer_quotes_only_what_needs_it() {
        let mut w = Writer::new(&["url", "note"]);
        w.row(["https://a.test", "plain"].into_iter());
        w.row(["https://b.test", "has,comma"].into_iter());
        w.row(["https://c.test", "has\"quote"].into_iter());
        w.row(["https://d.test", " padded "].into_iter());
        let out = w.finish();
        assert_eq!(
            out,
            "url,note\r\n\
             https://a.test,plain\r\n\
             https://b.test,\"has,comma\"\r\n\
             https://c.test,\"has\"\"quote\"\r\n\
             https://d.test,\" padded \"\r\n"
        );
    }

    #[test]
    fn what_the_writer_writes_the_reader_reads_back() {
        let hard = ["a,b", "c\"d", "e\nf", " g ", ""];
        let mut w = Writer::new(&["one", "two", "three", "four", "five"]);
        w.row(hard.into_iter());
        let text = w.finish();
        let t = Table::parse(&text);
        assert_eq!(t.rows().len(), 1);
        assert_eq!(t.rows()[0], hard);
    }
}
