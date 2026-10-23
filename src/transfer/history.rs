//! History as CSV: cryo's own backup format, and the one shape a
//! spreadsheet can read.
//!
//! No other browser exports browsing history as a file you can hand to
//! another browser — Chromium keeps it in a live database, Firefox in
//! its own. So this format is cryo's: it writes what the omnibox ranks
//! on (`url,title,visits,last_visit`) and reads the same file back, so a
//! profile can move between machines without losing its memory.
//!
//! `last_visit` is written as a UTC timestamp a human can read, and read
//! back either as that or as bare unix seconds — a column someone edited
//! in Excel still imports.

use crate::persist::Entry;
use crate::transfer::csv::{Table, Writer};
use crate::transfer::TransferError;

const URL_COLUMNS: [&str; 3] = ["url", "address", "text"];
const TITLE_COLUMNS: [&str; 2] = ["title", "name"];
const VISIT_COLUMNS: [&str; 4] = ["visits", "visit count", "uses", "count"];
const TIME_COLUMNS: [&str; 4] = ["last visit", "last seen", "date", "last visit time"];

const EXPORT_HEADER: [&str; 4] = ["url", "title", "visits", "last_visit"];

/// Read a history CSV into entries. Rows with no url are counted, not
/// fatal: the caller reports them.
pub fn parse(text: &str) -> Result<(Vec<Entry>, usize), TransferError> {
    let table = Table::parse(text);
    let url = table
        .column(&URL_COLUMNS)
        .ok_or_else(|| TransferError::Columns("no url column".into()))?;
    let title = table.column(&TITLE_COLUMNS);
    let visits = table.column(&VISIT_COLUMNS);
    let time = table.column(&TIME_COLUMNS);

    let mut out = Vec::with_capacity(table.rows().len());
    let mut skipped = 0;
    for row in 0..table.rows().len() {
        let title = table.field(row, title);
        let entry = Entry::new(
            table.field(row, Some(url)),
            Some(title).filter(|t| !t.is_empty()),
            table.field(row, visits).parse().unwrap_or(1),
            parse_time(table.field(row, time)),
        );
        match entry {
            Ok(e) => out.push(e),
            Err(_) => skipped += 1,
        }
    }
    Ok((out, skipped))
}

/// Render remembered visits, most-visited first — the order the omnibox
/// ranks them in, so the file reads like the browser behaves.
pub fn render(entries: &[Entry]) -> String {
    let mut rows: Vec<&Entry> = entries.iter().collect();
    rows.sort_by(|a, b| (b.uses, b.last_seen).cmp(&(a.uses, a.last_seen)));
    let mut out = Writer::new(&EXPORT_HEADER);
    for e in rows {
        out.row(
            [
                e.text.clone(),
                e.title.clone().unwrap_or_default(),
                e.uses.to_string(),
                format_time(e.last_seen),
            ]
            .iter()
            .map(String::as_str)
            .collect::<Vec<_>>()
            .into_iter(),
        );
    }
    out.finish()
}

/// Does this text look like a history export?
pub fn looks_like(text: &str) -> bool {
    let head = text.lines().next().unwrap_or_default();
    let table = Table::parse(head);
    table.column(&URL_COLUMNS).is_some()
        && (table.column(&VISIT_COLUMNS).is_some() || table.column(&TIME_COLUMNS).is_some())
}

/// `1970-01-01T00:00:00Z`. Seconds since the epoch, formatted without a
/// date library: the civil-calendar arithmetic below is exact for every
/// year this program will see.
fn format_time(unix: u64) -> String {
    let days = (unix / 86_400) as i64;
    let secs = unix % 86_400;
    let (y, m, d) = civil_from_days(days);
    format!(
        "{y:04}-{m:02}-{d:02}T{h:02}:{min:02}:{s:02}Z",
        h = secs / 3_600,
        min = (secs % 3_600) / 60,
        s = secs % 60,
    )
}

/// Read back what `format_time` wrote, or bare seconds, or nothing.
/// Anything unreadable becomes 0 — an undated visit, which ranks last
/// rather than ranking as "now".
fn parse_time(raw: &str) -> u64 {
    let raw = raw.trim();
    if raw.is_empty() {
        return 0;
    }
    if let Ok(secs) = raw.parse::<u64>() {
        return secs;
    }
    // `YYYY-MM-DDTHH:MM:SSZ`, and the space-separated form Excel writes.
    let (date, time) = match raw.split_once(['T', ' ']) {
        Some((date, time)) => (date, time.trim_end_matches('Z')),
        None => (raw, ""),
    };
    let mut parts = date.split('-');
    let (Some(y), Some(m), Some(d)) = (parts.next(), parts.next(), parts.next()) else {
        return 0;
    };
    let (Ok(y), Ok(m), Ok(d)) = (y.parse::<i64>(), m.parse::<u32>(), d.parse::<u32>()) else {
        return 0;
    };
    if !(1..=12).contains(&m) || !(1..=31).contains(&d) {
        return 0;
    }
    let mut clock = time.split(':').map(|p| p.parse::<u64>().unwrap_or(0));
    let secs = clock.next().unwrap_or(0) * 3_600
        + clock.next().unwrap_or(0) * 60
        + clock.next().unwrap_or(0);
    let days = days_from_civil(y, m, d);
    if days < 0 {
        return 0;
    }
    (days as u64) * 86_400 + secs
}

/// Days since 1970-01-01 for a civil date, and back. Howard Hinnant's
/// `days_from_civil` / `civil_from_days`: proleptic Gregorian, exact,
/// and small enough to keep instead of a date dependency.
fn days_from_civil(y: i64, m: u32, d: u32) -> i64 {
    let y = y - i64::from(m <= 2);
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let mp = i64::from(if m > 2 { m - 3 } else { m + 9 });
    let doy = (153 * mp + 2) / 5 + i64::from(d) - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = (if mp < 10 { mp + 3 } else { mp - 9 }) as u32;
    (y + i64::from(m <= 2), m, d)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn timestamps_round_trip_through_the_wire_format() {
        for secs in [
            0,
            1,
            86_399,
            86_400,
            951_782_400,   // 2000-02-29, a leap day
            1_600_000_000, // 2020-09-13T12:26:40Z
            4_102_444_800, // 2100-01-01, not a leap year
        ] {
            assert_eq!(parse_time(&format_time(secs)), secs, "{secs}");
        }
    }

    #[test]
    fn the_wire_format_is_the_one_people_expect() {
        assert_eq!(format_time(0), "1970-01-01T00:00:00Z");
        assert_eq!(format_time(1_600_000_000), "2020-09-13T12:26:40Z");
        assert_eq!(format_time(951_782_400), "2000-02-29T00:00:00Z");
    }

    #[test]
    fn hand_edited_timestamps_still_read() {
        assert_eq!(parse_time("1600000000"), 1_600_000_000);
        assert_eq!(parse_time("2020-09-13 12:26:40"), 1_600_000_000);
        assert_eq!(parse_time("2020-09-13"), 1_599_955_200);
        assert_eq!(parse_time(""), 0);
        assert_eq!(parse_time("whenever"), 0);
        assert_eq!(parse_time("2020-13-99"), 0);
        assert_eq!(parse_time("1900-01-01"), 0, "before the epoch has no rank");
    }

    #[test]
    fn reads_its_own_export_back() {
        let entries = vec![
            Entry::new("https://a.test", Some("A"), 9, 1_600_000_000).unwrap(),
            Entry::new("https://b.test", None, 1, 0).unwrap(),
        ];
        let csv = render(&entries);
        assert!(csv.starts_with("url,title,visits,last_visit\r\n"));
        let (back, skipped) = parse(&csv).unwrap();
        assert_eq!(skipped, 0);
        assert_eq!(back, entries);
    }

    #[test]
    fn export_puts_the_busiest_rows_first() {
        let entries = vec![
            Entry::new("https://quiet.test", None, 1, 10).unwrap(),
            Entry::new("https://busy.test", None, 50, 1).unwrap(),
        ];
        let csv = render(&entries);
        let busy = csv.find("busy.test").unwrap();
        let quiet = csv.find("quiet.test").unwrap();
        assert!(busy < quiet);
    }

    #[test]
    fn titles_with_commas_and_quotes_survive() {
        let entries = vec![Entry::new("https://a.test", Some("A, \"the\" site"), 1, 5).unwrap()];
        let (back, _) = parse(&render(&entries)).unwrap();
        assert_eq!(back[0].title.as_deref(), Some("A, \"the\" site"));
    }

    #[test]
    fn rows_with_no_url_are_counted_not_fatal() {
        let csv = "url,title,visits,last_visit\n\
                   https://a.test,A,1,1970-01-01T00:00:01Z\n\
                   ,orphan,1,\n";
        let (rows, skipped) = parse(csv).unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(skipped, 1);
    }

    #[test]
    fn a_file_without_a_url_column_is_refused() {
        assert!(matches!(
            parse("title,visits\nA,1\n"),
            Err(TransferError::Columns(_))
        ));
    }

    #[test]
    fn a_missing_visit_count_reads_as_one_visit() {
        let (rows, _) = parse("url\nhttps://a.test\n").unwrap();
        assert_eq!(rows[0].uses, 1);
        assert_eq!(rows[0].last_seen, 0);
    }

    #[test]
    fn looks_like_wants_a_url_and_something_about_visiting_it() {
        assert!(looks_like("url,title,visits,last_visit\n"));
        assert!(looks_like("url,last_visit\n"));
        assert!(!looks_like("url,username,password\n"));
        assert!(!looks_like("name,url,username,password,note\n"));
        assert!(!looks_like(""));
    }
}
