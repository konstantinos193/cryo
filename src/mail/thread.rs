//! Threading: which rows of one listing are one conversation. A domain
//! service (AGENTS §4) — it takes envelopes and returns a key per row;
//! nothing here touches the server or the page.
//!
//! Messages join through the ids they cite: a row's `In-Reply-To` and
//! `References` name earlier messages, and two rows citing the same id
//! are in one thread even when that id itself is not in the window
//! (the root may have scrolled out of the newest 500, or sit in Sent).
//! Rows with no citations fall back to the subject: a "Re: x" joins an
//! "x" already listed. That is the useful half of the JWZ algorithm;
//! the dummy-container tree it builds is not needed when the page only
//! wants to group and sort.

use std::collections::HashMap;

use super::Envelope;

/// One key per envelope, index-aligned: the uid of the thread's
/// earliest message, as text. Rows sharing a key are one conversation.
pub fn keys(envelopes: &[Envelope]) -> Vec<String> {
    let n = envelopes.len();
    let mut sets = Sets::new(n);

    // Pass one: cited ids. A citation of a listed message joins the two
    // rows; a citation of an unlisted one joins every row that cites it.
    let mut by_id: HashMap<&str, usize> = HashMap::new();
    for (i, e) in envelopes.iter().enumerate() {
        if let Some(id) = normalize_id(&e.message_id) {
            by_id.entry(id).or_insert(i);
        }
    }
    let mut by_citation: HashMap<&str, usize> = HashMap::new();
    for (i, e) in envelopes.iter().enumerate() {
        for cited in citations(e) {
            if let Some(&j) = by_id.get(cited) {
                sets.union(i, j);
            }
            match by_citation.get(cited) {
                Some(&j) => sets.union(i, j),
                None => {
                    by_citation.insert(cited, i);
                }
            }
        }
    }

    // Pass two: a reply with nothing to cite joins the subject it
    // answers, if that subject is listed. Non-replies never merge on
    // subject alone — two "Invoice" mails from two senders are two
    // conversations.
    let subjects: Vec<(String, bool)> =
        envelopes.iter().map(|e| base_subject(&e.subject)).collect();
    let mut by_subject: HashMap<&str, usize> = HashMap::new();
    // Originals register first, whatever order the listing has them
    // in, so a reply listed above its original still finds it.
    for (i, (base, is_reply)) in subjects.iter().enumerate() {
        if !is_reply && !base.is_empty() {
            by_subject.entry(base).or_insert(i);
        }
    }
    for (i, (base, is_reply)) in subjects.iter().enumerate() {
        if !is_reply || base.is_empty() || citations(&envelopes[i]).next().is_some() {
            continue;
        }
        match by_subject.get(base.as_str()) {
            Some(&j) => sets.union(i, j),
            // No original listed: later replies to the same subject
            // join this one.
            None => {
                by_subject.insert(base, i);
            }
        }
    }

    // The key is the root's uid — the member with the earliest date,
    // ties and dateless rows broken by the smallest uid.
    let mut root: HashMap<usize, usize> = HashMap::new();
    for i in 0..n {
        let set = sets.find(i);
        let best = root.entry(set).or_insert(i);
        if earlier(&envelopes[i], &envelopes[*best]) {
            *best = i;
        }
    }
    (0..n)
        .map(|i| envelopes[root[&sets.find(i)]].uid.to_string())
        .collect()
}

fn earlier(a: &Envelope, b: &Envelope) -> bool {
    match (a.date > 0, b.date > 0) {
        (true, true) => (a.date, a.uid) < (b.date, b.uid),
        (true, false) => true,
        (false, true) => false,
        (false, false) => a.uid < b.uid,
    }
}

/// `<id>` and `id` are the same id; blanks are no id at all.
fn normalize_id(raw: &str) -> Option<&str> {
    let id = raw
        .trim()
        .trim_start_matches('<')
        .trim_end_matches('>')
        .trim();
    (!id.is_empty()).then_some(id)
}

/// Every id a row cites — `References` first, then `In-Reply-To`.
fn citations(e: &Envelope) -> impl Iterator<Item = &str> {
    e.references
        .split_whitespace()
        .chain(e.in_reply_to.split_whitespace())
        .filter_map(normalize_id)
}

/// The subject with every reply/forward prefix stripped, lower-cased,
/// and whether any prefix was there — "Re: Fwd: Plans" → ("plans",
/// true).
fn base_subject(subject: &str) -> (String, bool) {
    const PREFIXES: [&str; 8] = ["re:", "fw:", "fwd:", "aw:", "sv:", "vs:", "απ:", "πρθ:"];
    let mut s = subject.trim();
    let mut stripped = false;
    loop {
        let lower = s.to_lowercase();
        let Some(p) = PREFIXES.iter().find(|p| lower.starts_with(*p)) else {
            break;
        };
        s = s[p.len()..].trim_start();
        stripped = true;
    }
    let base: String = s
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase();
    (base, stripped)
}

/// Union-find over row indexes.
struct Sets(Vec<usize>);

impl Sets {
    fn new(n: usize) -> Self {
        Self((0..n).collect())
    }

    fn find(&mut self, mut i: usize) -> usize {
        while self.0[i] != i {
            self.0[i] = self.0[self.0[i]];
            i = self.0[i];
        }
        i
    }

    fn union(&mut self, a: usize, b: usize) {
        let (a, b) = (self.find(a), self.find(b));
        if a != b {
            self.0[a.max(b)] = a.min(b);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env(uid: u32, id: &str, refs: &str, subject: &str, date: i64) -> Envelope {
        Envelope {
            uid,
            message_id: id.into(),
            references: refs.into(),
            subject: subject.into(),
            date,
            ..Envelope::default()
        }
    }

    #[test]
    fn citations_join_rows_and_the_earliest_names_the_thread() {
        let rows = [
            env(30, "<c@x>", "<a@x> <b@x>", "Re: plans", 300),
            env(20, "<b@x>", "<a@x>", "Re: plans", 200),
            env(10, "<a@x>", "", "plans", 100),
            env(5, "<z@x>", "", "other", 50),
        ];
        assert_eq!(keys(&rows), ["10", "10", "10", "5"]);
    }

    #[test]
    fn a_shared_citation_of_an_unlisted_root_still_joins() {
        let rows = [
            env(2, "<b@x>", "<gone@x>", "Re: x", 200),
            env(1, "<a@x>", "<gone@x>", "Re: x", 100),
        ];
        assert_eq!(keys(&rows), ["1", "1"]);
    }

    #[test]
    fn subject_joins_replies_only() {
        let rows = [
            env(3, "<c@x>", "", "Re: Invoice", 300),
            env(2, "<b@x>", "", "Invoice", 200),
            env(1, "<a@x>", "", "Invoice", 100),
        ];
        // The reply joins the first "Invoice" listed; the two bare
        // "Invoice" rows stay apart.
        let k = keys(&rows);
        assert_eq!(k[0], k[1]);
        assert_ne!(k[1], k[2]);
    }

    #[test]
    fn prefixes_strip_repeatedly_and_dateless_rows_sort_last() {
        assert_eq!(
            base_subject(" RE: fwd:  Plans  for  June "),
            ("plans for june".into(), true)
        );
        assert_eq!(base_subject("Plans"), ("plans".into(), false));
        let rows = [
            env(9, "<b@x>", "<a@x>", "re: p", 0),
            env(4, "<a@x>", "", "p", 0),
        ];
        assert_eq!(keys(&rows), ["4", "4"]);
    }
}
