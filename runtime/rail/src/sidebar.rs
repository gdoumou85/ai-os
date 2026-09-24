//! Words the sidebar's pages show (sidebar design §3), apart from GTK so they can be tested.

/// What a click on a project puts in the message box.
pub fn continue_text(name: &str) -> String { format!("let's continue {name}") }

/// How long ago, as a person says it.
pub fn ago(now: i64, then: i64) -> String {
    if then <= 0 { return "a while ago".into() }
    match (now - then).max(0) / 86_400 {
        0 => "today".into(),
        1 => "yesterday".into(),
        d if d < 60 => format!("{d} days ago"),
        d => format!("{} months ago", d / 30),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ago_says_it_as_a_person_would() {
        let now = 100 * 86_400;
        assert_eq!(ago(now, now - 5), "today");
        assert_eq!(ago(now, now - 86_400 - 5), "yesterday");
        assert_eq!(ago(now, now - 3 * 86_400), "3 days ago");
        assert_eq!(ago(now, now - 90 * 86_400), "3 months ago");
        assert_eq!(ago(now, 0), "a while ago");
        assert_eq!(continue_text("Pool Game"), "let's continue Pool Game");
    }
}
