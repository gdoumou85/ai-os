use crate::find::{self, Found, Kind};
use crate::moves::Move;
use crate::schema;
use std::cell::RefCell;

/// One message of the conversation before the newest one (one-loop design §1).
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Msg { pub role: String, pub content: String }

#[derive(Debug, Clone, PartialEq, Default)]
pub struct Prompt {
    pub system: String,
    /// The newest user-side message; the screen picture, if any, rides on it.
    pub user: String,
    pub allowed: Vec<&'static str>,
    /// A PNG for the model to see with the user message: the screen hand's latest look (2b).
    pub image: Option<Vec<u8>>,
    /// The chat before `user`, oldest first.
    pub history: Vec<Msg>,
    /// The model cannot see: the screen actions are left out of the grammar.
    pub no_screen: bool,
}

#[derive(Debug, thiserror::Error)]
pub enum ModelError {
    #[error("model http: {0}")] Http(String),
    /// 402 or 429: the account's allowance is used up for now; the cloud pool moves on.
    #[error("model allowance used up: {0}")] Quota(String),
    #[error("model answer was not a valid move: {0}")] BadJson(String),
    /// Stop, pressed while the answer came: the stream was dropped where it stood.
    #[error("the answer was stopped")] Stopped,
    /// The runner stopped the answer at its length limit: said as that, not as a bad format.
    #[error("the answer was cut off at the model's length limit, {0} words in")] CutOff(usize),
    /// The answer went on and on with nothing to read: blank space, the loop a model held to a
    /// JSON format can fall into (the owner's 27B, 2026-09-25: 40,000 tokens, 33 words, 4 hours).
    #[error("the answer got stuck writing blank space, {0} words in")] Stuck(usize),
    #[error("fake model has no more scripted moves")] Exhausted,
}

/// The one thing the loop needs from a model: given a prompt, one move. Swappable (1b spec §5).
pub trait Model {
    fn next_move(&self, prompt: &Prompt) -> Result<Move, ModelError>;
    /// `next_move`, with `watch` told how many words of the answer have come, every few seconds
    /// while it comes; `watch` answering false drops the answer where it stands (Stop). A model
    /// that does not stream answers in one go and never calls it.
    fn next_move_watched(&self, prompt: &Prompt, watch: &mut dyn FnMut(usize) -> bool) -> Result<Move, ModelError> {
        let _ = watch;
        self.next_move(prompt)
    }
    /// The context the model is run with, in tokens: what a step's evidence has to fit in. The
    /// desktop hand's look cap follows it (2a §4).
    fn context_tokens(&self) -> usize { 8192 }
    /// Whether the model takes pictures (one-loop design §2): only then is the screen offered.
    fn sees(&self) -> bool { false }
    /// Something the owner should be told about the model itself, once: the cloud pool switched
    /// to another model on its own.
    fn news(&self) -> Option<String> { None }
}

/// Scripted moves for tests; records every prompt it was given.
/// `unreadable`: how many of the next answers come back as text that is not a move, the way a
/// runner that does not enforce the grammar answers (LM Studio with a thinking Qwen, 2026-09-19).
pub struct FakeModel { queue: RefCell<std::collections::VecDeque<Move>>, pub prompts: RefCell<Vec<Prompt>>, pub unreadable: std::cell::Cell<u32>, pub sees: std::cell::Cell<bool> }

impl FakeModel {
    pub fn new(moves: Vec<Move>) -> Self { Self { queue: RefCell::new(moves.into()), prompts: RefCell::new(vec![]), unreadable: Default::default(), sees: Default::default() } }
}

impl Model for FakeModel {
    fn next_move(&self, prompt: &Prompt) -> Result<Move, ModelError> {
        self.prompts.borrow_mut().push(prompt.clone());
        // The real grammar forces a learn in the learning turn. A script that has none there
        // answers "exhausted" and keeps its next move for the next job's prompt.
        if prompt.allowed == ["learn"] && !matches!(self.queue.borrow().front(), Some(Move::Learn { .. })) {
            return Err(ModelError::Exhausted);
        }
        if self.unreadable.get() > 0 {
            self.unreadable.set(self.unreadable.get() - 1);
            return Err(ModelError::BadJson(r#"missing field `move`: {"understood":"…","act":{}}"#.into()));
        }
        self.queue.borrow_mut().pop_front().ok_or(ModelError::Exhausted)
    }
    fn sees(&self) -> bool { self.sees.get() }
}

/// The context assumed until the Model card says otherwise. Ollama is asked for `loaded_context()`,
/// the same number the rest of the machine plans against, so the two can never drift apart.
const NUM_CTX: usize = 8192;

/// What the model was actually loaded with. LM Studio and the cloud runners are never told a
/// context size — theirs is whatever the server loaded — so 8192 was only ever a guess about
/// them, and it capped `look` at 40 controls on a model holding four times that. Set
/// `AI_OS_CONTEXT` to the window the model really has.
fn loaded_context() -> usize { parse_context(std::env::var("AI_OS_CONTEXT").ok()) }

/// Nonsense (empty, words, a window smaller than the one the rules were written for) falls back
/// to `NUM_CTX`. The Model card holds the same floor.
fn parse_context(v: Option<String>) -> usize {
    v.and_then(|s| s.trim().parse().ok()).filter(|n| *n >= NUM_CTX).unwrap_or(NUM_CTX)
}

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
    /// Whether this runner sees, asked once per process and then kept (see `sees()`).
    pub sees: std::cell::OnceCell<bool>,
}

impl RemoteModel {
    /// A runner at a known address that is never looked for elsewhere.
    pub fn at(kind: Kind, url: &str, model: &str) -> Self {
        Self { kind, url: RefCell::new(url.into()), model: model.into(), key: None, finder: Box::new(Vec::new), sees: std::cell::OnceCell::new() }
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
    fn ask_at(&self, url: &str, prompt: &Prompt, watch: &mut dyn FnMut(usize) -> bool) -> Result<Move, (bool, ModelError)> {
        let (endpoint, body) = match self.kind {
            Kind::Ollama => (format!("{url}/api/chat"), ollama_body(&self.model, prompt)),
            Kind::OpenAi => (format!("{url}/v1/chat/completions"), openai_body(&self.model, prompt)),
        };
        let mut req = ureq::AgentBuilder::new().timeout_read(answer_timeout()).timeout_write(answer_timeout()).build().post(&endpoint);
        if let Some(k) = &self.key { req = req.set("Authorization", &format!("Bearer {k}")); }
        match req.send_json(body) {
            Ok(r) => read_stream(self.kind, r.into_reader(), std::time::Duration::from_secs(2), watch).and_then(|c| parse_content(&c)).map_err(|e| (false, e)),
            Err(ureq::Error::Transport(t)) if t.kind() == ureq::ErrorKind::ConnectionFailed => Err((true, ModelError::Http(t.to_string()))),
            // The runner's own reason, not just its number: a 402 from a signed-in Ollama means a
            // cloud model's allowance ran out, and only the body says so.
            Err(ureq::Error::Status(code, r)) => {
                let why = format!("{endpoint}: status {code}: {}", runner_reason(&r.into_string().unwrap_or_default()));
                Err((false, if matches!(code, 402 | 429) { ModelError::Quota(why) } else { ModelError::Http(why) }))
            }
            Err(e) => Err((false, ModelError::Http(e.to_string()))),
        }
    }
}

/// How long one answer may take: 30 minutes, or `AI_OS_MODEL_TIMEOUT` seconds. Three minutes was
/// too short for the owner's bigger LM Studio model on a long job (2026-09-19): a model that must
/// load first, or a machine with no graphics card, can take several minutes over one answer.
/// Since answers stream (v0.11.0) this is the silence allowed — a model loading, a long prompt
/// being read — not the whole answer: a slow model writing a big file goes on as long as words
/// keep coming.
fn answer_timeout() -> std::time::Duration {
    std::time::Duration::from_secs(std::env::var("AI_OS_MODEL_TIMEOUT").ok().and_then(|s| s.parse().ok()).filter(|s| *s > 0).unwrap_or(1800))
}

/// The reason in a runner's error body: Ollama's `{"error":"…"}`, OpenAI's `{"error":{"message":"…"}}`,
/// or the text itself, cut short.
fn runner_reason(body: &str) -> String {
    let v: serde_json::Value = serde_json::from_str(body).unwrap_or_default();
    let why = v["error"]["message"].as_str().or(v["error"].as_str()).unwrap_or(body.trim());
    why.chars().take(300).collect()
}

/// Narrow `format.oneOf` to the moves legal for this call (decision 13): the model physically
/// cannot answer out of turn. `$defs` (the action schema, shared by `act`/`done`) is untouched.
/// Empty `allowed` keeps every move — used for calls with no state to narrow against.
///
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

/// The schema narrowed to `allowed`, and with the screen actions dropped for a blind model.
fn format_for(prompt: &Prompt) -> serde_json::Value {
    let s = narrow_schema(schema::value(), &prompt.allowed);
    if prompt.no_screen { schema::without_screen(s) } else { s }
}

/// `prompt.history` as chat messages, oldest first — the conversation between the system prompt
/// and the newest `user` message.
fn history(prompt: &Prompt) -> impl Iterator<Item = serde_json::Value> + '_ {
    prompt.history.iter().map(|m| serde_json::json!({ "role": m.role, "content": m.content }))
}

pub fn ollama_body(model: &str, prompt: &Prompt) -> serde_json::Value {
    let mut user = serde_json::json!({ "role": "user", "content": prompt.user });
    if let Some(png) = &prompt.image { user["images"] = serde_json::json!([base64(png)]); }
    serde_json::json!({
        "model": model,
        "stream": true,
        "think": false,
        "format": format_for(prompt),
        // Ollama loads the model at what each request asks for, so a fixed 8192 here undid the
        // Model card's bar on every question (the owner, 2026-09-23). No `num_predict`: Ollama's own
        // default is no length limit, and `read_stream` cuts an answer bigger than the window.
        // Asking for the window's size broke ollama.com, whose models cap what one answer may be
        // (nemotron-3-super: 65536 < 131072, the owner, 2026-09-25).
        "options": { "temperature": 0.0, "num_ctx": loaded_context() },
        "messages": std::iter::once(serde_json::json!({ "role": "system", "content": prompt.system }))
            .chain(history(prompt)).chain(std::iter::once(user)).collect::<Vec<_>>()
    })
}

pub fn parse_ollama(resp: &serde_json::Value) -> Result<Move, ModelError> {
    parse_content(resp["message"]["content"].as_str().unwrap_or(""))
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
        "stream": true,
        "temperature": 0.0,
        "response_format": { "type": "json_schema", "json_schema": {
            "name": "move", "strict": true, "schema": format_for(prompt) } },
        "messages": std::iter::once(serde_json::json!({ "role": "system", "content": prompt.system }))
            .chain(history(prompt)).chain(std::iter::once(serde_json::json!({ "role": "user", "content": content }))).collect::<Vec<_>>()
    })
}

pub fn parse_openai(resp: &serde_json::Value) -> Result<Move, ModelError> {
    parse_content(resp["choices"][0]["message"]["content"].as_str().unwrap_or(""))
}

/// A move from the whole of an answer's text.
pub fn parse_content(content: &str) -> Result<Move, ModelError> {
    serde_json::from_str(content).map_err(|e| ModelError::BadJson(format!("{e}: {content}")))
}

/// A 429, however the runner shapes it — `error.code`, a top-level `code`, or just words in the
/// message — is the account's allowance, not a plain failure: the cloud pool moves to the next
/// account on `Quota`, same as a 402/429 HTTP status in `ask_at`. Anything else in `error` is `Http`.
fn stream_error(v: &serde_json::Value) -> Option<ModelError> {
    let why = v["error"].as_str().or(v["error"]["message"].as_str())?;
    let is_429 = v["error"]["code"] == 429 || v["code"] == 429 || why.contains("429");
    Some(if is_429 { ModelError::Quota(why.to_string()) } else { ModelError::Http(why.to_string()) })
}

/// The whole body as one JSON object, for a runner that answered `stream: true` with its ordinary
/// non-streamed response instead — LM Studio does this under some settings, and so does a proxy
/// that buffers the whole thing before replying. Each kind's answer sits where its non-streamed
/// response always put it. `None` when the body isn't even that; a top-level `error` there is
/// honoured as `Http`, same as today's in-stream check.
fn whole_body_content(kind: Kind, body: &str) -> Result<Option<String>, ModelError> {
    let Ok(v) = serde_json::from_str::<serde_json::Value>(body.trim()) else { return Ok(None) };
    if let Some(why) = v["error"].as_str().or(v["error"]["message"].as_str()) { return Err(ModelError::Http(why.to_string())) }
    let content = match kind {
        Kind::Ollama => v["message"]["content"].as_str(),
        Kind::OpenAi => v["choices"][0]["message"]["content"].as_str(),
    };
    Ok(content.map(str::to_string))
}

/// An answer as it streams in, whole: Ollama sends a JSON object per line, an OpenAI-style runner
/// `data: ` lines ending in `data: [DONE]`. `watch` hears the words so far at most every `every`
/// and drops the stream by answering false. A silence longer than the read timeout is an error
/// that says "timed out", which `next_move_watched` asks once more after. Lines that never parse
/// as a stream item are kept instead (capped, so a chatty non-JSON proxy can't grow forever); when
/// none ever parsed, that raw body is tried once as a whole, non-streamed answer — a runner that
/// ignored `stream: true` and just sent its ordinary response still answers. Only when that also
/// yields nothing is it said as what it is, with the body's start.
pub fn read_stream(kind: Kind, from: impl std::io::Read, every: std::time::Duration, watch: &mut dyn FnMut(usize) -> bool) -> Result<String, ModelError> {
    use std::io::BufRead;
    let cap = loaded_context() * 4;
    let (mut content, mut seen, mut raw) = (String::new(), false, String::new());
    let mut told = std::time::Instant::now();
    // Pieces in a row that added nothing to read: no letter of the answer and no thought.
    let mut blank = 0;
    for line in std::io::BufReader::new(from).lines() {
        let line = line.map_err(|e| ModelError::Http(match e.kind() {
            std::io::ErrorKind::TimedOut | std::io::ErrorKind::WouldBlock => format!("timed out: the model said nothing for {} s", answer_timeout().as_secs()),
            _ => e.to_string(),
        }))?;
        let json = match kind { Kind::Ollama => Some(line.as_str()), Kind::OpenAi => line.strip_prefix("data:").map(str::trim).filter(|d| *d != "[DONE]") };
        let Some(v) = json.and_then(|j| serde_json::from_str::<serde_json::Value>(j).ok()) else {
            if raw.len() < cap { raw.push_str(&line); raw.push('\n'); }
            continue
        };
        seen = true;
        if let Some(e) = stream_error(&v) { return Err(e) }
        let cut = match kind { Kind::Ollama => v["done_reason"] == "length", Kind::OpenAi => v["choices"][0]["finish_reason"] == "length" };
        let piece = match kind { Kind::Ollama => &v["message"]["content"], Kind::OpenAi => &v["choices"][0]["delta"]["content"] };
        if let Some(p) = piece.as_str() { content.push_str(p); }
        // A thinking model's thoughts come with empty answer pieces; those are not blank.
        let thought = match kind { Kind::Ollama => &v["message"]["thinking"], Kind::OpenAi => { let d = &v["choices"][0]["delta"]; if d["reasoning_content"].is_string() { &d["reasoning_content"] } else { &d["reasoning"] } } };
        let said = |x: &serde_json::Value| x.as_str().is_some_and(|t| !t.trim().is_empty());
        blank = if said(piece) || said(thought) { 0 } else { blank + 1 };
        // ponytail: 300 pieces, a few minutes on a slow model; no real answer has that many blanks in a row.
        if blank > 300 { return Err(ModelError::Stuck(content.split_whitespace().count())) }
        // Bigger than the whole context window could never be kept in the chat anyway, so this is
        // the window, not a limit on how much the AI may write — and it stops a model repeating
        // itself forever, which the learning turn has no Stop button to catch (engine.rs's `learn`).
        if content.len() / 4 > loaded_context() { return Err(ModelError::CutOff(content.split_whitespace().count())) }
        if cut { return Err(ModelError::CutOff(content.split_whitespace().count())) }
        if told.elapsed() >= every {
            told = std::time::Instant::now();
            if !watch(content.split_whitespace().count()) { return Err(ModelError::Stopped) }
        }
    }
    if !seen {
        if let Some(whole) = whole_body_content(kind, &raw)? { return Ok(whole) }
        return Err(ModelError::Http(format!("the runner's answer was not a stream: {}", raw.chars().take(300).collect::<String>().trim())))
    }
    Ok(content)
}

impl Model for RemoteModel {
    fn next_move(&self, prompt: &Prompt) -> Result<Move, ModelError> { self.next_move_watched(prompt, &mut |_| true) }

    fn next_move_watched(&self, prompt: &Prompt, watch: &mut dyn FnMut(usize) -> bool) -> Result<Move, ModelError> {
        let url = self.url.borrow().clone();
        match self.ask_at(&url, prompt, watch) {
            Err((true, gone)) => {
                // The same model at another address: the router handed the runner's machine a new one.
                let Some(moved) = (self.finder)().into_iter()
                    .find(|f| f.kind == self.kind && f.model.as_deref() == Some(self.model.as_str()) && f.url != url)
                else { return Err(gone) };
                eprintln!("the model {} moved: {url} -> {}", self.model, moved.url);
                *self.url.borrow_mut() = moved.url.clone();
                self.ask_at(&moved.url, prompt, watch).map_err(|(_, e)| e)
            }
            // A read that timed out, once: the owner's VM slept mid-request and woke to a dead
            // wait, and LM Studio reloading an unloaded model can outlast one wait too.
            // A silence mid-answer says "timed out" too (`read_stream`), and is asked once more.
            // ponytail: matched on ureq's wording; untested, since a test would sit out the whole answer timeout.
            Err((false, ModelError::Http(e))) if e.contains("timed out") => {
                eprintln!("the model did not answer in time ({e}); asking once more");
                self.ask_at(&url, prompt, watch).map_err(|(_, e)| e)
            }
            answer => answer.map_err(|(_, e)| e),
        }
    }

    fn context_tokens(&self) -> usize { loaded_context() }

    /// Ollama says so in `/api/show`; an OpenAI-style runner does not, so `AI_OS_MODEL_SEES=1`
    /// is how one that can is marked. ponytail: asked once per process; a model swapped under a
    /// running engine keeps the first answer until the engine restarts (the Model card restarts it).
    fn sees(&self) -> bool {
        *self.sees.get_or_init(|| match self.kind {
            Kind::Ollama => {
                let url = self.url.borrow().clone();
                ureq::AgentBuilder::new().timeout(std::time::Duration::from_secs(10)).build()
                    .post(&format!("{url}/api/show")).send_json(serde_json::json!({ "model": self.model }))
                    .ok().and_then(|r| r.into_json::<serde_json::Value>().ok())
                    .is_some_and(|v| v["capabilities"].as_array().is_some_and(|c| c.iter().any(|x| x == "vision")))
            }
            Kind::OpenAi => std::env::var("AI_OS_MODEL_SEES").is_ok_and(|v| v.trim() == "1"),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p() -> Prompt { Prompt { system: "sys".into(), user: "hello".into(), allowed: vec![], image: None, ..Default::default() } }

    use crate::testing::{closed_port, json_response, serve};

    const OLLAMA_HI: &str = r#"{"message":{"role":"assistant","content":"{\"move\":\"reply\",\"text\":\"hi\"}"}}"#;
    const OPENAI_HI: &str = r#"{"choices":[{"message":{"role":"assistant","content":"{\"move\":\"reply\",\"text\":\"hi\"}"}}]}"#;

    #[test]
    fn a_runners_error_says_why() {
        assert_eq!(runner_reason(r#"{"error":"you have reached your weekly usage limit"}"#), "you have reached your weekly usage limit");
        assert_eq!(runner_reason(r#"{"error":{"message":"invalid key","type":"auth"}}"#), "invalid key");
        assert_eq!(runner_reason("  Payment Required
"), "Payment Required");
    }

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
        let b = openai_body("bonsai-8b", &Prompt { system: "s".into(), user: "hello".into(), allowed: vec!["reply", "ask"], image: None, ..Default::default() });
        assert_eq!(b["model"], "bonsai-8b");
        assert_eq!(b["stream"], true);
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
        let with = Prompt { system: "s".into(), user: "look".into(), allowed: vec![], image: Some(b"png".to_vec()), ..Default::default() };
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
        let chunk = serde_json::json!({"choices":[{"delta":{"content": r#"{"move":"reply","text":"hi"}"#}}]});
        let addr = serve(json_response("200 OK", &format!("data: {chunk}\n\ndata: [DONE]\n")), 1);
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
            Move::Reply { thought: String::new(), text: "one".into(), outcome: crate::moves::Ending::Done },
            Move::Reply { thought: String::new(), text: "two".into(), outcome: crate::moves::Ending::Done },
        ]);
        assert!(matches!(m.next_move(&p()).unwrap(), Move::Reply { text, .. } if text == "one"));
        assert!(matches!(m.next_move(&p()).unwrap(), Move::Reply { text, .. } if text == "two"));
        assert!(matches!(m.next_move(&p()), Err(ModelError::Exhausted)));
        assert_eq!(m.prompts.borrow().len(), 3);
        assert_eq!(m.prompts.borrow()[0].user, "hello");
    }

    #[test]
    fn the_fake_never_spends_a_scripted_move_on_a_learning_turn_that_is_not_learn() {
        let m = FakeModel::new(vec![Move::Reply { thought: String::new(), text: "next job's move".into(), outcome: crate::moves::Ending::Done }]);
        let learn_only = Prompt { system: String::new(), user: String::new(), allowed: vec!["learn"], image: None, ..Default::default() };
        assert!(m.next_move(&learn_only).is_err());
        assert!(matches!(m.next_move(&learn_only.clone()), Err(_)));
        let any = Prompt { allowed: vec![], ..learn_only };
        assert!(matches!(m.next_move(&any), Ok(Move::Reply { .. })), "still there for the next prompt");
    }

    #[test]
    fn ollama_request_is_grammar_forced_and_deterministic() {
        let b = ollama_body("qwen3.5:9b", &p());
        assert_eq!(b["model"], "qwen3.5:9b");
        assert_eq!(b["stream"], true);
        assert_eq!(b["think"], false);
        assert_eq!(b["options"]["temperature"], 0.0);
        assert_eq!(b["options"]["num_ctx"], 8192);
        assert!(b["options"]["num_predict"].is_null(), "no length limit asked: a cloud model refuses one past its own cap");
        assert_eq!(b["format"]["oneOf"].as_array().unwrap().len(), 6);
        assert_eq!(b["messages"][0]["role"], "system");
        assert_eq!(b["messages"][1]["content"], "hello");
    }

    #[test]
    fn a_stated_context_wins_and_nonsense_falls_back() {
        assert_eq!(parse_context(Some("32768".into())), 32768);
        assert_eq!(parse_context(Some(" 16384 ".into())), 16384);
        for bad in [None, Some("".into()), Some("lots".into()), Some("4096".into()), Some("-1".into())] {
            assert_eq!(parse_context(bad), NUM_CTX);
        }
    }

    #[test]
    fn the_ollama_connection_says_its_context_and_sends_the_same_number() {
        let m = RemoteModel::local("x");
        assert_eq!(m.context_tokens(), 8192);
        let b = ollama_body("x", &Prompt { system: String::new(), user: String::new(), allowed: vec![], image: None, ..Default::default() });
        assert_eq!(b["options"]["num_ctx"], m.context_tokens());
        assert_eq!(FakeModel::new(vec![]).context_tokens(), 8192, "the trait default");
    }

    #[test]
    fn ollama_body_narrows_the_schema_to_allowed_moves() {
        let narrowed = Prompt { system: "s".into(), user: "u".into(), allowed: vec!["reply", "ask"], image: None, ..Default::default() };
        let b = ollama_body("m", &narrowed);
        let one_of = b["format"]["oneOf"].as_array().unwrap();
        assert_eq!(one_of.len(), 2);
        let names: Vec<&str> = one_of.iter().map(|e| e["properties"]["move"]["enum"][0].as_str().unwrap()).collect();
        assert_eq!(names, ["reply", "ask"]);
        assert!(b["format"]["$defs"].is_object(), "$defs must survive narrowing");

        let all = Prompt { system: "s".into(), user: "u".into(), allowed: vec![], image: None, ..Default::default() };
        let b2 = ollama_body("m", &all);
        assert_eq!(b2["format"]["oneOf"].as_array().unwrap().len(), 6);
    }

    #[test]
    fn a_streamed_answer_is_read_whole_from_either_runner() {
        let o = [r#"{"message":{"content":"{\"move\":\"reply\",\"thought\":\"t\","},"done":false}"#,
                 r#"{"message":{"content":"\"text\":\"hi\",\"outcome\":\"done\"}"},"done":true}"#].join("\n");
        let got = read_stream(Kind::Ollama, o.as_bytes(), std::time::Duration::ZERO, &mut |_| true).unwrap();
        assert!(matches!(parse_content(&got).unwrap(), Move::Reply { text, .. } if text == "hi"), "{got}");
        let s = "data: {\"choices\":[{\"delta\":{\"content\":\"{\\\"move\\\":\"}}]}\n\ndata: {\"choices\":[{\"delta\":{\"content\":\"\\\"reply\\\",\\\"thought\\\":\\\"t\\\",\\\"text\\\":\\\"hi\\\",\\\"outcome\\\":\\\"done\\\"}\"}}]}\n\ndata: [DONE]\n";
        let got = read_stream(Kind::OpenAi, s.as_bytes(), std::time::Duration::ZERO, &mut |_| true).unwrap();
        assert!(matches!(parse_content(&got).unwrap(), Move::Reply { text, .. } if text == "hi"), "{got}");
    }

    #[test]
    fn stop_drops_a_stream_and_a_runner_error_is_said() {
        let o = r#"{"message":{"content":"one two three"},"done":false}"#;
        let mut heard = 0;
        assert!(matches!(read_stream(Kind::Ollama, o.as_bytes(), std::time::Duration::ZERO, &mut |w| { heard = w; false }), Err(ModelError::Stopped)));
        assert_eq!(heard, 3);
        let e = r#"{"error":"model runner has unexpectedly stopped"}"#;
        assert!(matches!(read_stream(Kind::Ollama, e.as_bytes(), std::time::Duration::ZERO, &mut |_| true), Err(ModelError::Http(w)) if w.contains("unexpectedly")));
    }

    #[test]
    fn an_answer_cut_at_the_length_limit_says_so() {
        let o = r#"{"message":{"content":"a b"},"done":true,"done_reason":"length"}"#;
        assert!(matches!(read_stream(Kind::Ollama, o.as_bytes(), std::time::Duration::ZERO, &mut |_| true), Err(ModelError::CutOff(2))));
        let s = r#"data: {"choices":[{"delta":{"content":"a b c"},"finish_reason":"length"}]}"#;
        assert!(matches!(read_stream(Kind::OpenAi, s.as_bytes(), std::time::Duration::ZERO, &mut |_| true), Err(ModelError::CutOff(3))));
    }

    #[test]
    fn an_answer_stuck_on_blank_space_is_dropped_but_thinking_is_not() {
        let mut body = r#"{"message":{"content":"{\"move\":\"act\","},"done":false}"#.to_string() + "\n";
        let thinking = format!("{}\n", r#"{"message":{"content":"","thinking":"hmm"},"done":false}"#).repeat(400);
        let words = r#"{"message":{"content":"\"thought\":\"x\"}"},"done":true}"#;
        let fine = body.clone() + &thinking + words;
        assert!(read_stream(Kind::Ollama, fine.as_bytes(), std::time::Duration::ZERO, &mut |_| true).is_ok(), "a long thought is not blank");
        body.push_str(&format!("{}\n", r#"{"message":{"content":"\n  "},"done":false}"#).repeat(301));
        assert!(matches!(read_stream(Kind::Ollama, body.as_bytes(), std::time::Duration::ZERO, &mut |_| true), Err(ModelError::Stuck(1))));
    }

    #[test]
    fn a_stream_bigger_than_the_context_window_is_cut_off() {
        // A model repeating itself forever would grow the content past what the window could ever
        // hold, one small piece at a time — nothing here ever says `done_reason: "length"`.
        let want = loaded_context() * 4 + 1;
        let piece = "a".repeat(1000);
        let (mut body, mut pushed) = (String::new(), 0);
        while pushed < want {
            body.push_str(&format!(r#"{{"message":{{"content":"{piece}"}},"done":false}}"#));
            body.push('\n');
            pushed += piece.len();
        }
        assert!(matches!(read_stream(Kind::Ollama, body.as_bytes(), std::time::Duration::ZERO, &mut |_| true), Err(ModelError::CutOff(_))));
    }

    #[test]
    fn a_runner_that_ignored_stream_still_answers_from_its_whole_body() {
        // OpenAI-style: one whole response object, not `data:` lines.
        let whole = r#"{"choices":[{"message":{"content":"{\"move\":\"reply\",\"thought\":\"t\",\"text\":\"hi\",\"outcome\":\"done\"}"}}]}"#;
        let got = read_stream(Kind::OpenAi, whole.as_bytes(), std::time::Duration::ZERO, &mut |_| true).unwrap();
        assert!(matches!(parse_content(&got).unwrap(), Move::Reply { text, .. } if text == "hi"), "{got}");

        // Ollama-style: same idea, its own whole shape.
        let whole = r#"{"message":{"content":"{\"move\":\"reply\",\"thought\":\"t\",\"text\":\"hi\",\"outcome\":\"done\"}"},"done":true}"#;
        let got = read_stream(Kind::Ollama, whole.as_bytes(), std::time::Duration::ZERO, &mut |_| true).unwrap();
        assert!(matches!(parse_content(&got).unwrap(), Move::Reply { text, .. } if text == "hi"), "{got}");
    }

    #[test]
    fn a_429_in_the_stream_moves_the_cloud_pool_on_instead_of_giving_up() {
        let s = r#"data: {"error":{"code":429,"message":"Rate limit exceeded"}}"#;
        assert!(matches!(read_stream(Kind::OpenAi, s.as_bytes(), std::time::Duration::ZERO, &mut |_| true), Err(ModelError::Quota(w)) if w.contains("Rate limit")));
    }

    #[test]
    fn parses_ollama_reply_content() {
        let resp = serde_json::json!({"message":{"role":"assistant","content":"{\"move\":\"reply\",\"text\":\"hi\"}"}});
        let m = parse_ollama(&resp).unwrap();
        assert!(matches!(m, Move::Reply { text, .. } if text == "hi"));
        let bad = serde_json::json!({"message":{"content":"not json"}});
        assert!(matches!(parse_ollama(&bad), Err(ModelError::BadJson(_))));
    }

    #[test]
    fn the_conversation_goes_between_the_system_and_the_newest_message() {
        let p = Prompt { system: "s".into(), user: "now".into(), history: vec![
            Msg { role: "user".into(), content: "show my projects".into() },
            Msg { role: "assistant".into(), content: r#"{"move":"reply"}"#.into() },
        ], ..Default::default() };
        for b in [ollama_body("m", &p), openai_body("m", &p)] {
            let m = b["messages"].as_array().unwrap();
            let roles: Vec<&str> = m.iter().map(|x| x["role"].as_str().unwrap()).collect();
            assert_eq!(roles, ["system", "user", "assistant", "user"]);
            assert_eq!(m[1]["content"], "show my projects");
        }
    }

    #[test]
    fn a_model_that_cannot_see_is_offered_no_screen_actions() {
        let p = Prompt { system: "s".into(), user: "u".into(), no_screen: true, ..Default::default() };
        let b = ollama_body("m", &p);
        let kinds: Vec<&str> = b["format"]["$defs"]["action"]["oneOf"].as_array().unwrap().iter().map(|o| o["properties"]["kind"]["enum"][0].as_str().unwrap()).collect();
        for k in crate::schema::SCREEN_KINDS { assert!(!kinds.contains(&k), "{k} offered to a blind model"); }
        assert!(kinds.contains(&"key") && kinds.contains(&"look"), "{kinds:?}");
        let o = openai_body("m", &p);
        assert!(o["response_format"]["json_schema"]["schema"]["$defs"]["action"]["oneOf"].as_array().unwrap().iter().all(|a| a["properties"]["kind"]["enum"][0] != "screen_look"));
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
            ..Default::default()
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
