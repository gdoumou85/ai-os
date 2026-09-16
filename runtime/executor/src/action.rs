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
}
