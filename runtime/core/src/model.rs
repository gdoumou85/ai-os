use crate::find::{self, Found, Kind};
use crate::moves::Move;
use crate::schema;
use std::cell::RefCell;

#[derive(Debug, Clone, PartialEq)]
pub struct Prompt {
    pub system: String,
    pub user: String,
    pub allowed: Vec<&'static str>,
    /// A PNG for the model to see with the user message: the screen hand's latest look (2b).
    pub image: Option<Vec<u8>>,
}

#[derive(Debug, thiserror::Error)]
pub enum ModelError {
    #[error("model http: {0}")] Http(String),
    #[error("model answer was not a valid move: {0}")] BadJson(String),
    #[error("fake model has no more scripted moves")] Exhausted,
}

/// The one thing the loop needs from a model: given a prompt, one move. Swappable (1b spec §5).
pub trait Model {
    fn next_move(&self, prompt: &Prompt) -> Result<Move, ModelError>;
    /// The context the model is run with, in tokens: what a step's evidence has to fit in. The
    /// desktop hand's look cap follows it (2a §4).
    fn context_tokens(&self) -> usize { 8192 }
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

/// The context Ollama is asked for and the one the rest of the machine plans against: one number,
/// so `ollama_body` and `context_tokens` can never drift apart.
const NUM_CTX: usize = 8192;

/// A model runner over HTTP (parent §4.3): Ollama, or LM Studio through its OpenAI-style door
/// (network-models spec §2). Grammar-forced, temperature 0, 8k context — the Phase 0 settings.
/// When its address stops answering, `finder` looks for the same model on the home network (§3).
pub struct RemoteModel {
    pub kind: Kind,
    /// A cell: a runner found again at a new address is kept for the rest of this process.
    pub url: RefCell<String>,
    pub model: String,
    pub key: Option<String>,
    pub finder: Box<dyn Fn() -> Vec<Found>>,
}

impl RemoteModel {
    /// A runner at a known address that is never looked for elsewhere.
    pub fn at(kind: Kind, url: &str, model: &str) -> Self {
        Self { kind, url: RefCell::new(url.into()), model: model.into(), key: None, finder: Box::new(Vec::new) }
    }
    pub fn local(model: &str) -> Self { Self::at(Kind::Ollama, "http://127.0.0.1:11434", model) }
    /// What the installer wrote into the unit: `AI_OS_MODEL_URL` (else this machine),
    /// `AI_OS_MODEL_KIND` (else ollama) and `AI_OS_MODEL_KEY` (from the private `model.env`).
    pub fn from_env(model: &str) -> Self {
        let kind = std::env::var("AI_OS_MODEL_KIND").ok().and_then(|k| Kind::parse(&k)).unwrap_or(Kind::Ollama);
        let url = std::env::var("AI_OS_MODEL_URL").unwrap_or_else(|_| "http://127.0.0.1:11434".into());
        let key = std::env::var("AI_OS_MODEL_KEY").ok().filter(|k| !k.is_empty());
        let finder_key = key.clone();
        Self {
            key,
            finder: Box::new(move || find::scan(&find::home_hosts(), find::OLLAMA_PORT, find::LMSTUDIO_PORT, finder_key.as_deref())),
            ..Self::at(kind, &url, model)
        }
    }

    /// One request to the runner at `url`. `Err((true, _))` when it could not be reached at all —
    /// the only failure worth looking for it elsewhere.
    fn ask_at(&self, url: &str, prompt: &Prompt) -> Result<Move, (bool, ModelError)> {
        let (endpoint, body) = match self.kind {
            Kind::Ollama => (format!("{url}/api/chat"), ollama_body(&self.model, prompt)),
            Kind::OpenAi => (format!("{url}/v1/chat/completions"), openai_body(&self.model, prompt)),
        };
        let mut req = ureq::AgentBuilder::new().timeout(std::time::Duration::from_secs(180)).build().post(&endpoint);
        if let Some(k) = &self.key { req = req.set("Authorization", &format!("Bearer {k}")); }
        let resp: serde_json::Value = match req.send_json(body) {
            Ok(r) => r.into_json().map_err(|e| (false, ModelError::Http(e.to_string())))?,
            Err(ureq::Error::Transport(t)) if t.kind() == ureq::ErrorKind::ConnectionFailed => return Err((true, ModelError::Http(t.to_string()))),
            Err(e) => return Err((false, ModelError::Http(e.to_string()))),
        };
        match self.kind { Kind::Ollama => parse_ollama(&resp), Kind::OpenAi => parse_openai(&resp) }.map_err(|e| (false, e))
    }
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

/// Standard base64 with padding, for a picture in a JSON body.
/// ponytail: a dozen lines instead of a crate for the one place that needs it.
pub fn base64(data: &[u8]) -> String {
    const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(data.len().div_ceil(3) * 4);
    for c in data.chunks(3) {
        let n = (u32::from(c[0]) << 16) | (u32::from(*c.get(1).unwrap_or(&0)) << 8) | u32::from(*c.get(2).unwrap_or(&0));
        for i in 0..4 {
            out.push(if i <= c.len() { T[(n >> (18 - 6 * i) & 63) as usize] as char } else { '=' });
        }
    }
    out
}

pub fn ollama_body(model: &str, prompt: &Prompt) -> serde_json::Value {
    let mut user = serde_json::json!({ "role": "user", "content": prompt.user });
    if let Some(png) = &prompt.image { user["images"] = serde_json::json!([base64(png)]); }
    serde_json::json!({
        "model": model,
        "stream": false,
        "think": false,
        "format": narrow_schema(schema::value(), &prompt.allowed),
        "options": { "temperature": 0.0, "num_ctx": NUM_CTX },
        "messages": [ { "role": "system", "content": prompt.system }, user ]
    })
}

pub fn parse_ollama(resp: &serde_json::Value) -> Result<Move, ModelError> {
    let content = resp["message"]["content"].as_str().unwrap_or("");
    serde_json::from_str(content).map_err(|e| ModelError::BadJson(format!("{e}: {content}")))
}

/// LM Studio's OpenAI-style request: the same narrowed schema, forced through `response_format`.
/// No context size: LM Studio fixes it when it loads the model (the installer asks for ≥ 8192).
pub fn openai_body(model: &str, prompt: &Prompt) -> serde_json::Value {
    // A picture makes the content a list of parts; without one it stays the plain string it was.
    let content = match &prompt.image {
        None => serde_json::json!(prompt.user),
        Some(png) => serde_json::json!([
            { "type": "text", "text": prompt.user },
            { "type": "image_url", "image_url": { "url": format!("data:image/png;base64,{}", base64(png)) } }
        ]),
    };
    serde_json::json!({
        "model": model,
        "stream": false,
        "temperature": 0.0,
        "response_format": { "type": "json_schema", "json_schema": {
            "name": "move", "strict": true, "schema": narrow_schema(schema::value(), &prompt.allowed) } },
        "messages": [ { "role": "system", "content": prompt.system }, { "role": "user", "content": content } ]
    })
}

pub fn parse_openai(resp: &serde_json::Value) -> Result<Move, ModelError> {
    let content = resp["choices"][0]["message"]["content"].as_str().unwrap_or("");
    serde_json::from_str(content).map_err(|e| ModelError::BadJson(format!("{e}: {content}")))
}

impl Model for RemoteModel {
    fn next_move(&self, prompt: &Prompt) -> Result<Move, ModelError> {
        let url = self.url.borrow().clone();
        match self.ask_at(&url, prompt) {
            Err((true, gone)) => {
                // The same model at another address: the router handed the runner's machine a new one.
                let Some(moved) = (self.finder)().into_iter()
                    .find(|f| f.kind == self.kind && f.model.as_deref() == Some(self.model.as_str()) && f.url != url)
                else { return Err(gone) };
                eprintln!("the model {} moved: {url} -> {}", self.model, moved.url);
                *self.url.borrow_mut() = moved.url.clone();
                self.ask_at(&moved.url, prompt).map_err(|(_, e)| e)
            }
            answer => answer.map_err(|(_, e)| e),
        }
    }

    fn context_tokens(&self) -> usize { NUM_CTX }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p() -> Prompt { Prompt { system: "sys".into(), user: "hello".into(), allowed: vec![], image: None } }

    use crate::testing::{closed_port, json_response, serve};

    const OLLAMA_HI: &str = r#"{"message":{"role":"assistant","content":"{\"move\":\"reply\",\"text\":\"hi\"}"}}"#;
    const OPENAI_HI: &str = r#"{"choices":[{"message":{"role":"assistant","content":"{\"move\":\"reply\",\"text\":\"hi\"}"}}]}"#;

    #[test]
    fn ollama_http_error_status_is_model_error_http() {
        let addr = serve("HTTP/1.1 500 Internal Server Error\r\nContent-Length: 0\r\nConnection: close\r\n\r\n".into(), 1);
        let m = RemoteModel::at(Kind::Ollama, &format!("http://{addr}"), "x");
        assert!(matches!(m.next_move(&p()), Err(ModelError::Http(_))));
    }

    #[test]
    fn ollama_non_json_body_is_model_error_http() {
        let addr = serve("HTTP/1.1 200 OK\r\nContent-Type: text/plain\r\nContent-Length: 8\r\nConnection: close\r\n\r\nnot json".into(), 1);
        let m = RemoteModel::at(Kind::Ollama, &format!("http://{addr}"), "x");
        assert!(matches!(m.next_move(&p()), Err(ModelError::Http(_))));
    }

    #[test]
    fn openai_request_is_grammar_forced_and_deterministic() {
        let b = openai_body("bonsai-8b", &Prompt { system: "s".into(), user: "hello".into(), allowed: vec!["reply", "start"], image: None });
        assert_eq!(b["model"], "bonsai-8b");
        assert_eq!(b["stream"], false);
        assert_eq!(b["temperature"], 0.0);
        assert_eq!(b["response_format"]["type"], "json_schema");
        assert_eq!(b["response_format"]["json_schema"]["strict"], true);
        assert_eq!(b["response_format"]["json_schema"]["schema"]["oneOf"].as_array().unwrap().len(), 2);
        assert_eq!(b["messages"][0]["role"], "system");
        assert_eq!(b["messages"][1]["content"], "hello");
    }

    #[test]
    fn base64_matches_the_standard_vectors() {
        for (i, o) in [("", ""), ("f", "Zg=="), ("fo", "Zm8="), ("foo", "Zm9v"), ("foob", "Zm9vYg=="), ("fooba", "Zm9vYmE="), ("foobar", "Zm9vYmFy")] {
            assert_eq!(base64(i.as_bytes()), o, "{i}");
        }
        assert_eq!(base64(&[0xff, 0xfe, 0xfd]), "//79");
    }

    #[test]
    fn a_picture_rides_with_the_user_message_to_both_kinds_of_runner() {
        let with = Prompt { system: "s".into(), user: "look".into(), allowed: vec![], image: Some(b"png".to_vec()) };
        let o = ollama_body("m", &with);
        assert_eq!(o["messages"][1]["images"], serde_json::json!(["cG5n"]));
        assert_eq!(o["messages"][1]["content"], "look");
        let a = openai_body("m", &with);
        assert_eq!(a["messages"][1]["content"][0], serde_json::json!({"type":"text","text":"look"}));
        assert_eq!(a["messages"][1]["content"][1]["image_url"]["url"], "data:image/png;base64,cG5n");
        // No picture: the bodies are exactly what they were before pictures existed.
        let without = Prompt { image: None, ..with };
        assert!(ollama_body("m", &without)["messages"][1].get("images").is_none());
        assert_eq!(openai_body("m", &without)["messages"][1]["content"], "look");
    }

    #[test]
    fn parses_openai_reply_content() {
        let m = parse_openai(&serde_json::from_str(OPENAI_HI).unwrap()).unwrap();
        assert!(matches!(m, Move::Reply { text, .. } if text == "hi"));
        let bad = serde_json::json!({"choices":[{"message":{"content":"not json"}}]});
        assert!(matches!(parse_openai(&bad), Err(ModelError::BadJson(_))));
    }

    #[test]
    fn an_lm_studio_answer_comes_back_as_a_move() {
        let addr = serve(json_response("200 OK", OPENAI_HI), 1);
        let mut m = RemoteModel::at(Kind::OpenAi, &format!("http://{addr}"), "bonsai-8b");
        m.key = Some("k".into());
        assert!(matches!(m.next_move(&p()).unwrap(), Move::Reply { text, .. } if text == "hi"));
    }

    #[test]
    fn a_runner_that_moved_is_found_again() {
        let addr = serve(json_response("200 OK", OLLAMA_HI), 1);
        let new_url = format!("http://{addr}");
        let mut m = RemoteModel::at(Kind::Ollama, &format!("http://127.0.0.1:{}", closed_port()), "bonsai");
        let there = new_url.clone();
        m.finder = Box::new(move || vec![
            Found { url: "http://10.9.9.9:11434".into(), kind: Kind::Ollama, model: Some("some-other-model".into()) },
            Found { url: there.clone(), kind: Kind::Ollama, model: Some("bonsai".into()) },
        ]);
        assert!(matches!(m.next_move(&p()).unwrap(), Move::Reply { text, .. } if text == "hi"));
        assert_eq!(*m.url.borrow(), new_url, "kept for the next request");
    }

    #[test]
    fn a_runner_that_is_gone_is_an_http_error() {
        let old = format!("http://127.0.0.1:{}", closed_port());
        let mut m = RemoteModel::at(Kind::Ollama, &old, "bonsai");
        m.finder = Box::new(|| vec![Found { url: "http://10.9.9.9:11434".into(), kind: Kind::OpenAi, model: Some("bonsai".into()) }]);
        assert!(matches!(m.next_move(&p()), Err(ModelError::Http(_))), "a different kind is not the same runner");
        assert_eq!(*m.url.borrow(), old);
    }

    #[test]
    fn an_error_from_a_runner_that_answered_does_not_start_a_search() {
        let addr = serve("HTTP/1.1 500 Internal Server Error\r\nContent-Length: 0\r\nConnection: close\r\n\r\n".into(), 1);
        let mut m = RemoteModel::at(Kind::Ollama, &format!("http://{addr}"), "x");
        m.finder = Box::new(|| panic!("searched after an answer"));
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
        assert_eq!(b["format"]["oneOf"].as_array().unwrap().len(), 9);
        assert_eq!(b["messages"][0]["role"], "system");
        assert_eq!(b["messages"][1]["content"], "hello");
    }

    #[test]
    fn the_ollama_connection_says_its_context_and_sends_the_same_number() {
        let m = RemoteModel::local("x");
        assert_eq!(m.context_tokens(), 8192);
        let b = ollama_body("x", &Prompt { system: String::new(), user: String::new(), allowed: vec![], image: None });
        assert_eq!(b["options"]["num_ctx"], m.context_tokens());
        assert_eq!(FakeModel::new(vec![]).context_tokens(), 8192, "the trait default");
    }

    #[test]
    fn ollama_body_narrows_the_schema_to_allowed_moves() {
        let narrowed = Prompt { system: "s".into(), user: "u".into(), allowed: vec!["reply", "start"], image: None };
        let b = ollama_body("m", &narrowed);
        let one_of = b["format"]["oneOf"].as_array().unwrap();
        assert_eq!(one_of.len(), 2);
        let names: Vec<&str> = one_of.iter().map(|e| e["properties"]["move"]["enum"][0].as_str().unwrap()).collect();
        assert_eq!(names, ["reply", "start"]);
        assert!(b["format"]["$defs"].is_object(), "$defs must survive narrowing");

        let all = Prompt { system: "s".into(), user: "u".into(), allowed: vec![], image: None };
        let b2 = ollama_body("m", &all);
        assert_eq!(b2["format"]["oneOf"].as_array().unwrap().len(), 9);
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
        let m = RemoteModel::local("qwen3.5:9b");
        let prompt = Prompt {
            system: "You answer with one move. For small talk use {\"move\":\"reply\",\"text\":...}.".into(),
            user: "hello, who are you?".into(),
            allowed: vec![],
            image: None,
        };
        let mv = m.next_move(&prompt).unwrap();
        eprintln!("{mv:?}");
        assert!(matches!(mv, Move::Reply { .. }));
    }

    /// The desktop edition's engine reaches a runner on another machine (desktop design §4): one
    /// environment setting, read in one place. Only this test touches the variable.
    #[test]
    fn the_model_address_comes_from_the_environment() {
        for v in ["AI_OS_MODEL_URL", "AI_OS_MODEL_KIND", "AI_OS_MODEL_KEY"] { std::env::remove_var(v); }
        let m = RemoteModel::from_env("m");
        assert_eq!((m.url.borrow().as_str(), m.kind, m.key.as_deref()), ("http://127.0.0.1:11434", Kind::Ollama, None));
        std::env::set_var("AI_OS_MODEL_URL", "http://192.168.2.7:1234");
        std::env::set_var("AI_OS_MODEL_KIND", "openai");
        std::env::set_var("AI_OS_MODEL_KEY", "sk-lm-1");
        let m = RemoteModel::from_env("bonsai-8b");
        for v in ["AI_OS_MODEL_URL", "AI_OS_MODEL_KIND", "AI_OS_MODEL_KEY"] { std::env::remove_var(v); }
        assert_eq!((m.url.borrow().as_str(), m.model.as_str()), ("http://192.168.2.7:1234", "bonsai-8b"));
        assert_eq!((m.kind, m.key.as_deref()), (Kind::OpenAi, Some("sk-lm-1")));
    }
}
