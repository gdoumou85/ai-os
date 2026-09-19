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
    HttpPost { url: String, body: String },
    Install { packages: Vec<String> },
    Remove { packages: Vec<String> },
    Service { name: String, #[serde(rename = "do")] action: ServiceDo },
    MakeDir { path: String },
    FetchPackages { manager: Manager, packages: Vec<String> },
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
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ServiceDo {
    Enable,
    Disable,
    Restart,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Manager {
    Pip,
    Npm,
    Cargo,
}

/// A package/service name safe to interpolate into a shell argv: `^[a-z0-9]([a-z0-9+.@_-]*[a-z0-9+])?$`.
/// May end in `+` (g++, libstdc++6) but never `-` (apt's remove suffix), `.`, `@`, or `_`.
pub fn valid_name(s: &str) -> bool {
    let b = s.as_bytes();
    if b.is_empty() {
        return false;
    }
    let is_first = |c: u8| c.is_ascii_lowercase() || c.is_ascii_digit();
    let is_mid = |c: u8| is_first(c) || matches!(c, b'+' | b'.' | b'@' | b'_' | b'-');
    let is_last = |c: u8| is_first(c) || c == b'+';
    if !is_first(b[0]) {
        return false;
    }
    if b.len() == 1 {
        return true;
    }
    if !is_last(b[b.len() - 1]) {
        return false;
    }
    b[1..b.len() - 1].iter().all(|&c| is_mid(c))
}

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
    fn parses_the_1c_actions() {
        let cases = [
            r#"{"kind":"install","packages":["cowsay"]}"#,
            r#"{"kind":"remove","packages":["cowsay"]}"#,
            r#"{"kind":"service","name":"nginx","do":"enable"}"#,
            r#"{"kind":"make_dir","path":"/data/work"}"#,
            r#"{"kind":"fetch_packages","manager":"pip","packages":["tabulate"]}"#,
            r#"{"kind":"set_setting","key":"projects_root","value":"/data/work"}"#,
        ];
        for c in cases {
            let a: Action = serde_json::from_str(c).unwrap_or_else(|e| panic!("{c}: {e}"));
            let back = serde_json::to_string(&a).unwrap();
            assert_eq!(a, serde_json::from_str::<Action>(&back).unwrap());
        }
        assert!(matches!(
            serde_json::from_str::<Action>(r#"{"kind":"service","name":"x","do":"enable"}"#).unwrap(),
            Action::Service { action: ServiceDo::Enable, .. }
        ));
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
    fn app_names_carry_dots_and_capitals_but_never_paths() {
        for ok in ["org.gnome.TextEditor", "org.gnome.Calculator", "libreoffice-writer", "gnome_calc2"] { assert!(valid_app_name(ok), "{ok}"); }
        for bad in ["", "../x", "a/b", "a b", ".hidden", "-x", "x;rm"] { assert!(!valid_app_name(bad), "{bad}"); }
    }

    #[test]
    fn names_are_validated() {
        assert!(valid_name("cowsay"));
        assert!(valid_name("libkf6-x.y+z"));
        assert!(!valid_name("-o"));
        assert!(!valid_name("a;b"));
        assert!(!valid_name("/tmp/x.service"));
        assert!(!valid_name(""));
        assert!(!valid_name("Cowsay"));
        assert!(valid_name("g++"));
        assert!(valid_name("libstdc++6"));
        assert!(!valid_name("sudo-"));
        assert!(valid_name("a"));
        assert!(!valid_name("a."));
    }
}
