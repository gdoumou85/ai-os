use crate::moves::Move;
use crate::schema;
use std::cell::RefCell;

#[derive(Debug, Clone, PartialEq)]
pub struct Prompt { pub system: String, pub user: String, pub allowed: Vec<&'static str> }

#[derive(Debug, thiserror::Error)]
pub enum ModelError {
    #[error("model http: {0}")] Http(String),
    #[error("model answer was not a valid move: {0}")] BadJson(String),
    #[error("fake model has no more scripted moves")] Exhausted,
}

/// The one thing the loop needs from a model: given a prompt, one move. Swappable (1b spec §5).
pub trait Model {
    fn next_move(&self, prompt: &Prompt) -> Result<Move, ModelError>;
}

/// Scripted moves for tests; records every prompt it was given.
pub struct FakeModel { queue: RefCell<std::collections::VecDeque<Move>>, pub prompts: RefCell<Vec<Prompt>> }

impl FakeModel {
    pub fn new(moves: Vec<Move>) -> Self { Self { queue: RefCell::new(moves.into()), prompts: RefCell::new(vec![]) } }
}

impl Model for FakeModel {
    fn next_move(&self, prompt: &Prompt) -> Result<Move, ModelError> {
        self.prompts.borrow_mut().push(prompt.clone());
        self.queue.borrow_mut().pop_front().ok_or(ModelError::Exhausted)
    }
}

/// Ollama over HTTP on this machine (parent §4.3): grammar-forced via `format`, thinking off,
/// temperature 0, 8k context — the Phase 0 settings.
pub struct OllamaModel { pub url: String, pub model: String }

impl OllamaModel {
    pub fn local(model: &str) -> Self { Self { url: "http://127.0.0.1:11434".into(), model: model.into() } }
}

/// Narrow `format.oneOf` to the moves legal for this call (decision 13): the model physically
/// cannot answer out of turn. `$defs` (the action schema, shared by `act`/`done`) is untouched.
/// Empty `allowed` keeps every move — used for calls with no state to narrow against.
///
/// `pub(crate)` so `prompt.rs`'s tests can prove `allowed_moves`' hand-typed move names actually
/// exist in `MOVE_SCHEMA` (a typo or a renamed move must never silently narrow to fewer moves, or
/// to none) — see `prompt::tests::every_allowed_list_matches_a_real_move`.
pub(crate) fn narrow_schema(mut schema: serde_json::Value, allowed: &[&'static str]) -> serde_json::Value {
    if allowed.is_empty() { return schema; }
    if let Some(one_of) = schema["oneOf"].as_array() {
        let kept: Vec<serde_json::Value> = one_of.iter()
            .filter(|entry| entry["properties"]["move"]["enum"][0].as_str().map(|m| allowed.contains(&m)).unwrap_or(false))
            .cloned()
            .collect();
        // A hand-typed `allowed` name that doesn't match any MOVE_SCHEMA entry would otherwise
        // silently shrink `oneOf` — even to empty, locking the model out of every move. Debug-only:
        // this is a static-data invariant (the two lists are hand-typed, not runtime input), so it
        // costs nothing in release and is exercised by every debug/test build.
        debug_assert_eq!(kept.len(), allowed.len(), "allowed move names must all exist in MOVE_SCHEMA: {allowed:?}");
        schema["oneOf"] = serde_json::Value::Array(kept);
    }
    schema
}

pub fn ollama_body(model: &str, prompt: &Prompt) -> serde_json::Value {
    serde_json::json!({
        "model": model,
        "stream": false,
        "think": false,
        "format": narrow_schema(schema::value(), &prompt.allowed),
        "options": { "temperature": 0.0, "num_ctx": 8192 },
        "messages": [
            { "role": "system", "content": prompt.system },
            { "role": "user", "content": prompt.user }
        ]
    })
}

pub fn parse_ollama(resp: &serde_json::Value) -> Result<Move, ModelError> {
    let content = resp["message"]["content"].as_str().unwrap_or("");
    serde_json::from_str(content).map_err(|e| ModelError::BadJson(format!("{e}: {content}")))
}

impl Model for OllamaModel {
    fn next_move(&self, prompt: &Prompt) -> Result<Move, ModelError> {
        let resp: serde_json::Value = ureq::AgentBuilder::new()
            .timeout(std::time::Duration::from_secs(180))
            .build()
            .post(&format!("{}/api/chat", self.url))
            .send_json(ollama_body(&self.model, prompt))
            .map_err(|e| ModelError::Http(e.to_string()))?
            .into_json()
            .map_err(|e| ModelError::Http(e.to_string()))?;
        parse_ollama(&resp)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p() -> Prompt { Prompt { system: "sys".into(), user: "hello".into(), allowed: vec![] } }

    /// Binds an ephemeral local socket, accepts one connection, reads up to the end of the
    /// request headers (the body is irrelevant to these tests), then writes back `response`
    /// verbatim. Returns the address before the client connects, since the listener is already
    /// bound and listening.
    fn respond_once(response: &'static str) -> std::net::SocketAddr {
        use std::io::{Read, Write};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut received = Vec::new();
            let mut buf = [0u8; 4096];
            while !received.windows(4).any(|w| w == b"\r\n\r\n") {
                match stream.read(&mut buf) {
                    Ok(0) | Err(_) => break,
                    Ok(n) => received.extend_from_slice(&buf[..n]),
                }
            }
            let _ = stream.write_all(response.as_bytes());
        });
        addr
    }

    #[test]
    fn ollama_http_error_status_is_model_error_http() {
        let addr = respond_once("HTTP/1.1 500 Internal Server Error\r\nContent-Length: 0\r\nConnection: close\r\n\r\n");
        let m = OllamaModel { url: format!("http://{addr}"), model: "x".into() };
        assert!(matches!(m.next_move(&p()), Err(ModelError::Http(_))));
    }

    #[test]
    fn ollama_non_json_body_is_model_error_http() {
        let addr = respond_once("HTTP/1.1 200 OK\r\nContent-Type: text/plain\r\nContent-Length: 8\r\nConnection: close\r\n\r\nnot json");
        let m = OllamaModel { url: format!("http://{addr}"), model: "x".into() };
        assert!(matches!(m.next_move(&p()), Err(ModelError::Http(_))));
    }

    #[test]
    fn fake_returns_moves_in_order_then_errors() {
        let m = FakeModel::new(vec![
            Move::Reply { text: "one".into(), remember: None },
            Move::Reply { text: "two".into(), remember: None },
        ]);
        assert!(matches!(m.next_move(&p()).unwrap(), Move::Reply { text, .. } if text == "one"));
        assert!(matches!(m.next_move(&p()).unwrap(), Move::Reply { text, .. } if text == "two"));
        assert!(matches!(m.next_move(&p()), Err(ModelError::Exhausted)));
        assert_eq!(m.prompts.borrow().len(), 3);
        assert_eq!(m.prompts.borrow()[0].user, "hello");
    }

    #[test]
    fn ollama_request_is_grammar_forced_and_deterministic() {
        let b = ollama_body("qwen3.5:9b", &p());
        assert_eq!(b["model"], "qwen3.5:9b");
        assert_eq!(b["stream"], false);
        assert_eq!(b["think"], false);
        assert_eq!(b["options"]["temperature"], 0.0);
        assert_eq!(b["options"]["num_ctx"], 8192);
        assert_eq!(b["format"]["oneOf"].as_array().unwrap().len(), 8);
        assert_eq!(b["messages"][0]["role"], "system");
        assert_eq!(b["messages"][1]["content"], "hello");
    }

    #[test]
    fn ollama_body_narrows_the_schema_to_allowed_moves() {
        let narrowed = Prompt { system: "s".into(), user: "u".into(), allowed: vec!["reply", "start"] };
        let b = ollama_body("m", &narrowed);
        let one_of = b["format"]["oneOf"].as_array().unwrap();
        assert_eq!(one_of.len(), 2);
        let names: Vec<&str> = one_of.iter().map(|e| e["properties"]["move"]["enum"][0].as_str().unwrap()).collect();
        assert_eq!(names, ["reply", "start"]);
        assert!(b["format"]["$defs"].is_object(), "$defs must survive narrowing");

        let all = Prompt { system: "s".into(), user: "u".into(), allowed: vec![] };
        let b2 = ollama_body("m", &all);
        assert_eq!(b2["format"]["oneOf"].as_array().unwrap().len(), 8);
    }

    #[test]
    fn parses_ollama_reply_content() {
        let resp = serde_json::json!({"message":{"role":"assistant","content":"{\"move\":\"reply\",\"text\":\"hi\"}"}});
        let m = parse_ollama(&resp).unwrap();
        assert!(matches!(m, Move::Reply { text, .. } if text == "hi"));
        let bad = serde_json::json!({"message":{"content":"not json"}});
        assert!(matches!(parse_ollama(&bad), Err(ModelError::BadJson(_))));
    }

    /// Live: needs Ollama with the model pulled. AI_OS_LIVE=1 cargo test -p aios-core live_ollama -- --nocapture
    #[test]
    fn live_ollama_returns_a_reply() {
        if std::env::var("AI_OS_LIVE").as_deref() != Ok("1") { eprintln!("skipped: AI_OS_LIVE=1"); return; }
        let m = OllamaModel::local("qwen3.5:9b");
        let prompt = Prompt {
            system: "You answer with one move. For small talk use {\"move\":\"reply\",\"text\":...}.".into(),
            user: "hello, who are you?".into(),
            allowed: vec![],
        };
        let mv = m.next_move(&prompt).unwrap();
        eprintln!("{mv:?}");
        assert!(matches!(mv, Move::Reply { .. }));
    }
}
