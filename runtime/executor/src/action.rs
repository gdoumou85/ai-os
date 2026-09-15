use serde::{Deserialize, Serialize};

/// A structured action the model proposes. Tagged JSON: {"kind":"run_command","argv":[...]}.
/// Adding a variant here is the only way to give the AI a new kind of hand.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Action {
    RunCommand { argv: Vec<String> },
    ReadFile { path: String },
    WriteFile { path: String, contents: String },
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
}
