use executor::action::Action;
use serde::{Deserialize, Serialize};

/// One move per model turn (1b spec §4). The loop enforces which moves are legal in which state.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "move", rename_all = "snake_case")]
pub enum Move {
    Reply {
        text: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        remember: Option<String>,
    },
    Start {
        project: String,
        new_project: bool,
        description: String,
        goal: String,
        creative: bool,
        understood: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        remember: Option<String>,
    },
    Ask { questions: Vec<String> },
    Plan { steps: Vec<String> },
    Act { step: usize, action: Action },
    Replan { steps: Vec<String>, why: String },
    Done { summary: String, check: Action },
    GiveUp { reason: String, missing: String },
}

#[cfg(test)]
mod tests {
    use super::*;
    use executor::action::Action;

    #[test]
    fn parses_every_move() {
        let cases = [
            r#"{"move":"reply","text":"hi"}"#,
            r#"{"move":"reply","text":"noted","remember":"always use python3"}"#,
            r#"{"move":"start","project":"primes","new_project":true,"description":"prime printer","goal":"print 10 primes","creative":false,"understood":"Starting a new project primes"}"#,
            r#"{"move":"ask","questions":["Which language?"]}"#,
            r#"{"move":"plan","steps":["write primes.py","run it"]}"#,
            r#"{"move":"act","step":1,"action":{"kind":"write_file","path":"primes.py","contents":"print(2)"}}"#,
            r#"{"move":"replan","steps":["use a loop instead"],"why":"recursion overflowed"}"#,
            r#"{"move":"done","summary":"printed them","check":{"kind":"run_command","argv":["python3","primes.py"]}}"#,
            r#"{"move":"give_up","reason":"no compiler","missing":"gcc"}"#,
        ];
        for c in cases {
            let m: Move = serde_json::from_str(c).unwrap_or_else(|e| panic!("{c}: {e}"));
            let back = serde_json::to_string(&m).unwrap();
            let again: Move = serde_json::from_str(&back).unwrap();
            assert_eq!(m, again);
        }
        let d: Move = serde_json::from_str(r#"{"move":"done","summary":"s","check":{"kind":"run_command","argv":["true"]}}"#).unwrap();
        assert!(matches!(d, Move::Done { check: Action::RunCommand { .. }, .. }));
    }

    #[test]
    fn rejects_unknown_move() {
        assert!(serde_json::from_str::<Move>(r#"{"move":"format_disk"}"#).is_err());
    }

    #[test]
    fn move_names_match_schema() {
        let v = crate::schema::value();
        let names: Vec<String> = v["oneOf"].as_array().unwrap().iter()
            .map(|o| o["properties"]["move"]["enum"][0].as_str().unwrap().to_string()).collect();
        assert_eq!(names, ["reply", "start", "ask", "plan", "act", "replan", "done", "give_up"]);
    }
}
