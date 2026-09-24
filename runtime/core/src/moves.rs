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

/// A line of the model's own to-do list (one-loop design §1).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TodoItem { pub text: String, #[serde(default)] pub done: bool }

/// How a reply after work ends its card: Finished, or Couldn't finish.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Ending { #[default] Done, CouldNot }

/// One move per model call (one-loop design §1). No state machine: every move is legal at every
/// call except `learn`, which only the learning turn after work asks for.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "move", rename_all = "snake_case")]
pub enum Move {
    /// Say something and end the turn.
    Reply { #[serde(default)] thought: String, text: String, #[serde(default)] outcome: Ending },
    /// One question, with buttons; ends the turn, and the answer is the user's next message.
    Ask { #[serde(default)] thought: String, question: String, #[serde(default)] options: Vec<String> },
    /// The to-do list, whole, each time it changes.
    Todo { #[serde(default)] thought: String, items: Vec<TodoItem> },
    Act { #[serde(default)] thought: String, action: Action },
    /// A standing instruction, kept only when the user's words ask for one.
    Remember { #[serde(default)] thought: String, text: String },
    /// Legal only in the learning turn after work (Phase 3 §4).
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

    #[test]
    fn parses_every_move() {
        for c in [
            r#"{"move":"reply","thought":"t","text":"hi","outcome":"done"}"#,
            r#"{"move":"reply","thought":"t","text":"no","outcome":"could_not"}"#,
            r#"{"move":"ask","thought":"t","question":"Which language?","options":["Python","Rust"]}"#,
            r#"{"move":"todo","thought":"t","items":[{"text":"write it","done":true},{"text":"run it","done":false}]}"#,
            r#"{"move":"act","thought":"t","action":{"kind":"run_command","argv":["ls"]}}"#,
            r#"{"move":"act","thought":"t","action":{"kind":"web_search","query":"flappy"}}"#,
            r#"{"move":"remember","thought":"t","text":"always use python3"}"#,
            r#"{"move":"learn","entries":[],"used":[],"wrong":[],"remove":[]}"#,
        ] {
            let m: Move = serde_json::from_str(c).unwrap_or_else(|e| panic!("{c}: {e}"));
            assert_eq!(serde_json::from_str::<Move>(&serde_json::to_string(&m).unwrap()).unwrap(), m);
        }
        let r: Move = serde_json::from_str(r#"{"move":"reply","text":"hi"}"#).unwrap();
        assert_eq!(r, Move::Reply { thought: String::new(), text: "hi".into(), outcome: Ending::Done });
    }

    #[test]
    fn rejects_the_old_moves() {
        for c in [r#"{"move":"start","project":"p"}"#, r#"{"move":"plan","steps":[]}"#, r#"{"move":"done","summary":"s"}"#] {
            assert!(serde_json::from_str::<Move>(c).is_err(), "{c}");
        }
    }

    #[test]
    fn move_names_match_schema() {
        let v = crate::schema::value();
        let names: Vec<String> = v["oneOf"].as_array().unwrap().iter().map(|o| o["properties"]["move"]["enum"][0].as_str().unwrap().to_string()).collect();
        assert_eq!(names, ["reply", "ask", "todo", "act", "remember", "learn"]);
    }
}
