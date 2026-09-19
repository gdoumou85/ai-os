use executor::action::Action;
use serde::{Deserialize, Serialize};

/// One notebook entry the learning turn proposes (Phase 3 §4). `steps` are job step numbers;
/// what is stored comes from those steps in the record, never from these words.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LearnEntry {
    pub notebook: String,
    pub topic: String,
    pub kind: String,
    pub text: String,
    #[serde(default)]
    pub steps: Vec<usize>,
    #[serde(default)]
    pub links: Vec<String>,
}

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
        /// The craft notebooks this job belongs to (Phase 3 §2); none for a plain errand.
        #[serde(default)]
        skills: Vec<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        remember: Option<String>,
    },
    Housekeep {
        goal: String,
        understood: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        remember: Option<String>,
    },
    /// `options[i]` are the answers to offer for `questions[i]` as buttons (the owner, 2026-09-19);
    /// empty for a question only the person can answer in words.
    Ask { questions: Vec<String>, #[serde(default)] options: Vec<Vec<String>> },
    Plan { steps: Vec<String> },
    Act { step: usize, action: Action },
    Replan { steps: Vec<String>, why: String },
    Done { summary: String, check: Action },
    GiveUp { reason: String, missing: String },
    /// Legal only in the learning turn after a job (Phase 3 §4).
    Learn {
        entries: Vec<LearnEntry>,
        #[serde(default)] used: Vec<String>,
        #[serde(default)] wrong: Vec<String>,
        #[serde(default)] remove: Vec<String>,
    },
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
            r#"{"move":"start","project":"ball","new_project":true,"description":"d","goal":"g","creative":false,"understood":"u","skills":["blender"]}"#,
            r#"{"move":"housekeep","goal":"prepare /data/work","understood":"Housekeeping: preparing /data/work"}"#,
            r#"{"move":"ask","questions":["Which language?"]}"#,
            r#"{"move":"ask","questions":["Which language?","Its name?"],"options":[["Python","Rust"],[]]}"#,
            r#"{"move":"plan","steps":["write primes.py","run it"]}"#,
            r#"{"move":"act","step":1,"action":{"kind":"write_file","path":"primes.py","contents":"print(2)"}}"#,
            r#"{"move":"act","step":1,"action":{"kind":"set_setting","key":"projects_root","value":"/data/work"}}"#,
            r#"{"move":"replan","steps":["use a loop instead"],"why":"recursion overflowed"}"#,
            r#"{"move":"done","summary":"printed them","check":{"kind":"run_command","argv":["python3","primes.py"]}}"#,
            r#"{"move":"give_up","reason":"no compiler","missing":"gcc"}"#,
            r#"{"move":"learn","entries":[{"notebook":"this computer","topic":"open a website","kind":"technique","text":"open_app firefox with the address","steps":[7],"links":[]}],"used":["this computer/open a website"],"wrong":[],"remove":[]}"#,
            r#"{"move":"learn","entries":[]}"#,
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
        assert_eq!(names, ["reply", "start", "housekeep", "ask", "plan", "act", "replan", "done", "give_up", "learn"]);
    }

    #[test]
    fn integer_fields_are_1_based_in_schema() {
        // step/from_line/lines back `usize` fields; a grammar-valid negative would fail
        // serde_json parsing, breaking "grammar-forced means always parses" (decision 13).
        let v = crate::schema::value();
        assert_eq!(v["oneOf"][5]["properties"]["step"]["minimum"], 1);
        assert_eq!(v["$defs"]["action"]["oneOf"][1]["properties"]["from_line"]["minimum"], 1);
        assert_eq!(v["$defs"]["action"]["oneOf"][1]["properties"]["lines"]["minimum"], 1);
    }
}
