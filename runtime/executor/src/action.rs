use serde::{Deserialize, Serialize};

/// A structured action the model proposes. Tagged JSON: {"kind":"run_command","argv":[...]}.
/// Adding a variant here is the only way to give the AI a new kind of hand.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Action {
    RunCommand { argv: Vec<String> },
    /// Optional window: 1-based `from_line`, at most `lines` lines. Without it the whole file, capped.
    ReadFile {
        path: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        from_line: Option<usize>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        lines: Option<usize>,
    },
    WriteFile { path: String, contents: String },
    /// Replace one exact passage. `find` must occur exactly once (rule 9: edit in place).
    EditFile { path: String, find: String, replace: String },
    SetSetting { key: String, value: String },
    // The desktop hand (2a design §3). Ids come from the worker's own table; `name` on a press is
    // the model echoing what `look` reported, verified by the worker before anything runs, so the
    // risk rule can read a name without leaving `classify` pure.
    /// No window: list the open windows. A window: its controls, narrowed by `find`.
    Look {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        window: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        find: Option<String>,
    },
    Press { control: u32, name: String },
    /// Appends at the end; `replace` clears the control first.
    Type {
        control: u32,
        text: String,
        #[serde(default)]
        replace: bool,
    },
    /// Windowed like `ReadFile`.
    Read {
        control: u32,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        from_line: Option<usize>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        lines: Option<usize>,
    },
    /// A desktop-entry id (`org.gnome.TextEditor`); on the invisible display unless `visible`.
    OpenApp {
        name: String,
        #[serde(default)]
        visible: bool,
    },
    // The screen fallback (2b design): pixels, for what the controls above cannot reach. Aiming
    // is a grid then a zoom, so a click names a cell it has just looked into and a spot inside it.
    /// No cell: the whole screen under a numbered 8 × 6 grid. A cell: that cell enlarged under 4 × 4.
    ScreenLook {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        cell: Option<u32>,
    },
    /// `name` is what the model says it is clicking — its own reading of the screen, for the risk rule.
    ScreenClick {
        cell: u32,
        spot: u32,
        name: String,
        #[serde(default)]
        double: bool,
    },
    /// Into whatever has the focus; Enter after when asked.
    ScreenType {
        text: String,
        #[serde(default)]
        enter: bool,
    },
    /// A page as text (one-loop design §1b), paged like `ReadFile`.
    WebRead {
        url: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        from_line: Option<usize>,
    },
    /// The top results of a web search: title, address, snippet.
    WebSearch { query: String },
    /// Runs in the background, outliving the turn; output to a log (one-loop design §1b).
    StartProgram { name: String, argv: Vec<String> },
    ProgramOutput {
        name: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        lines: Option<usize>,
    },
    StopProgram { name: String },
    /// A key or combination on the visible screen: "ctrl+s", "alt+tab", "escape".
    Key { keys: String },
    /// The wheel at a spot of a square from the latest look.
    Scroll {
        cell: u32,
        spot: u32,
        direction: String,
        #[serde(default = "one")]
        amount: u32,
    },
    Drag { from_cell: u32, from_spot: u32, to_cell: u32, to_spot: u32 },
    /// Waits, then says what changed (one-loop design §1b); the engine's own.
    Wait { seconds: u32 },
    /// Wakes the AI later (watchers design §1): a timer, a check or a live program, with the
    /// reason for whoever handles the alert. The engine's own.
    Watch {
        name: String,
        reason: String,
        #[serde(default)]
        urgent: bool,
        when: String,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        command: Vec<String>,
    },
    Unwatch { name: String },
}

fn one() -> u32 { 1 }

/// A desktop-entry id: `^[A-Za-z0-9][A-Za-z0-9._-]*$`. Dots and capitals are normal there
/// (`org.gnome.TextEditor`); a slash, a space or a leading dot or dash never is.
pub fn valid_app_name(s: &str) -> bool {
    let b = s.as_bytes();
    !b.is_empty() && b[0].is_ascii_alphanumeric() && b.iter().all(|&c| c.is_ascii_alphanumeric() || matches!(c, b'.' | b'_' | b'-'))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_run_command() {
        let a: Action = serde_json::from_str(r#"{"kind":"run_command","argv":["ls","-la"]}"#).unwrap();
        assert_eq!(a, Action::RunCommand { argv: vec!["ls".into(), "-la".into()] });
    }

    #[test]
    fn rejects_unknown_kind() {
        let r: Result<Action, _> = serde_json::from_str(r#"{"kind":"format_disk"}"#);
        assert!(r.is_err(), "unknown action kinds must not deserialize");
    }

    #[test]
    fn read_file_window_is_optional() {
        let a: Action = serde_json::from_str(r#"{"kind":"read_file","path":"a.py"}"#).unwrap();
        assert_eq!(a, Action::ReadFile { path: "a.py".into(), from_line: None, lines: None });
        let b: Action = serde_json::from_str(r#"{"kind":"read_file","path":"a.py","from_line":10,"lines":20}"#).unwrap();
        assert_eq!(b, Action::ReadFile { path: "a.py".into(), from_line: Some(10), lines: Some(20) });
    }

    #[test]
    fn parses_edit_file() {
        let a: Action = serde_json::from_str(r#"{"kind":"edit_file","path":"a.py","find":"x = 1","replace":"x = 2"}"#).unwrap();
        assert_eq!(a, Action::EditFile { path: "a.py".into(), find: "x = 1".into(), replace: "x = 2".into() });
    }

    #[test]
    fn the_removed_kinds_no_longer_parse() {
        for k in ["install", "remove", "service", "make_dir", "fetch_packages", "http_post"] {
            assert!(serde_json::from_str::<Action>(&format!(r#"{{"kind":"{k}"}}"#)).is_err(), "{k}");
        }
        let a: Action = serde_json::from_str(r#"{"kind":"set_setting","key":"projects_root","value":"/data/work"}"#).unwrap();
        assert_eq!(a, Action::SetSetting { key: "projects_root".into(), value: "/data/work".into() });
    }

    #[test]
    fn desktop_actions_round_trip_with_kind_first() {
        let a = Action::Press { control: 7, name: "Bold".into() };
        let s = serde_json::to_string(&a).unwrap();
        assert!(s.starts_with(r#"{"kind":"press""#), "{s}");
        assert_eq!(serde_json::from_str::<Action>(&s).unwrap(), a);
        let t: Action = serde_json::from_str(r#"{"kind":"type","control":3,"text":"hi"}"#).unwrap();
        assert_eq!(t, Action::Type { control: 3, text: "hi".into(), replace: false });
        let o: Action = serde_json::from_str(r#"{"kind":"open_app","name":"org.gnome.Calculator"}"#).unwrap();
        assert_eq!(o, Action::OpenApp { name: "org.gnome.Calculator".into(), visible: false });
        let l: Action = serde_json::from_str(r#"{"kind":"look"}"#).unwrap();
        assert_eq!(l, Action::Look { window: None, find: None });
        let r: Action = serde_json::from_str(r#"{"kind":"read","control":9,"from_line":5,"lines":20}"#).unwrap();
        assert_eq!(r, Action::Read { control: 9, from_line: Some(5), lines: Some(20) });
    }

    #[test]
    fn a_watch_needs_no_command() {
        let a: Action = serde_json::from_str(r#"{"kind":"watch","name":"time","reason":"say the time","urgent":false,"when":"every 2 minutes"}"#).unwrap();
        assert_eq!(a, Action::Watch { name: "time".into(), reason: "say the time".into(), urgent: false, when: "every 2 minutes".into(), command: vec![] });
        let u: Action = serde_json::from_str(r#"{"kind":"unwatch","name":"time"}"#).unwrap();
        assert_eq!(u, Action::Unwatch { name: "time".into() });
    }

    #[test]
    fn app_names_carry_dots_and_capitals_but_never_paths() {
        for ok in ["org.gnome.TextEditor", "org.gnome.Calculator", "libreoffice-writer", "gnome_calc2"] { assert!(valid_app_name(ok), "{ok}"); }
        for bad in ["", "../x", "a/b", "a b", ".hidden", "-x", "x;rm"] { assert!(!valid_app_name(bad), "{bad}"); }
    }
}
