//! Words the sidebar's pages show (sidebar design §3), apart from GTK so they can be tested.
use aios_proto::WatcherView;

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

/// A watcher's line on the Watchers page: its name, when, who made it and when it last fired.
pub fn watcher_line(w: &WatcherView, now: i64) -> String {
    let last = if w.last_fired_at <= 0 { "never fired".to_string() } else { format!("last fired {}", ago(now, w.last_fired_at)) };
    format!("{} · {} · made by {} · {last}{}", w.name, w.when, w.made_by, if w.paused { " · paused" } else { "" })
}

/// Its dot, as a CSS class: red urgent, green on, grey paused.
pub fn watcher_dot(w: &WatcherView) -> &'static str {
    if w.paused { "dot-off" } else if w.urgent { "dot-urgent" } else { "dot-on" }
}

/// The menu entry, with the alerts since the page was last opened.
pub fn watchers_label(unseen: usize) -> String {
    if unseen == 0 { "🔔 Watchers".into() } else { format!("🔔 Watchers ({unseen})") }
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

    #[test]
    fn a_watcher_reads_as_a_line_with_its_dot() {
        let w = WatcherView { name: "time".into(), reason: "say it".into(), urgent: false, made_by: "you".into(), when: "every 2 min".into(), paused: false, last_fired_at: 0, last_text: String::new() };
        assert_eq!(watcher_line(&w, 100), "time · every 2 min · made by you · never fired");
        assert_eq!(watcher_dot(&w), "dot-on");
        let off = WatcherView { paused: true, urgent: true, last_fired_at: 100 * 86_400 - 5, ..w.clone() };
        assert_eq!(watcher_line(&off, 100 * 86_400), "time · every 2 min · made by you · last fired today · paused");
        assert_eq!(watcher_dot(&off), "dot-off");
        assert_eq!(watcher_dot(&WatcherView { urgent: true, ..w }), "dot-urgent");
        assert_eq!(watchers_label(0), "🔔 Watchers");
        assert_eq!(watchers_label(3), "🔔 Watchers (3)");
    }
}
