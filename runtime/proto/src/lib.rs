//! The wire between the engine service and its front doors (1d design §2, §3.2): typed events
//! out, plain text in. No engine types here — a client (the rail) must build without the
//! engine's dependencies.
use serde::{Deserialize, Serialize};
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::path::Path;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FileKind { Text, Image, Other }

impl FileKind {
    pub fn of(path: &str) -> FileKind {
        let ext = Path::new(path).extension().and_then(|e| e.to_str()).map(|e| e.to_ascii_lowercase()).unwrap_or_default();
        match ext.as_str() {
            "png" | "jpg" | "jpeg" | "gif" | "webp" | "bmp" | "svg" => FileKind::Image,
            "txt" | "md" | "py" | "rs" | "sh" | "js" | "ts" | "json" | "toml" | "yaml" | "yml" | "html" | "css" | "csv" | "c" | "h" | "cpp" | "go" | "java" | "rb" | "sql" | "ini" | "cfg" | "conf" | "log" | "xml" => FileKind::Text,
            _ => FileKind::Other,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ChangedFile { pub path: String, pub kind: FileKind, pub size: u64 }

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StepView { pub plan_step: usize, pub text: String, pub ok: bool }

/// What the open job is waiting for. `"none"` on the wire when nothing.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Waiting { None, Answer { questions: Vec<String>, #[serde(default)] options: Vec<Vec<String>> } }

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct JobState {
    pub id: String,
    pub name: String,
    pub housekeeping: bool,
    pub understood: String,
    pub plan: Vec<String>,
    pub steps: Vec<StepView>,
    pub waiting: Waiting,
}

/// One entry as the Skills screen draws it (Phase 3 §6).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct NoteView { pub topic: String, pub kind: String, pub text: String, pub uses: i64, pub failed: bool, pub needs_check: bool }

/// One notebook ("this computer" or a craft) and its entries, most-used first.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Notebook { pub name: String, pub entries: Vec<NoteView> }

/// One event, `kind` first (the same first-key rule the move grammar lives by).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Event {
    Said { text: String },
    /// Echo of a client's `say`, added by the service so every client shows it.
    You { text: String },
    Understood { job_id: String, name: String, text: String, housekeeping: bool },
    Plan { job_id: String, steps: Vec<String> },
    Step { job_id: String, plan_step: usize, text: String, ok: bool },
    /// `options[i]`: the answers to offer as buttons for `questions[i]`, maybe none.
    NeedsAnswer { job_id: String, questions: Vec<String>, #[serde(default)] options: Vec<Vec<String>> },
    Done { job_id: String, text: String, check: Option<String>, files: Vec<ChangedFile>, #[serde(default)] windows: Vec<String> },
    Failed { job_id: String, text: String, files: Vec<ChangedFile> },
    Stopped { job_id: String, text: String, files: Vec<ChangedFile> },
    /// What the learning turn after a job kept (Phase 3 §6). `pending`: something waits for Keep.
    Learned { job_id: String, lines: Vec<String>, #[serde(default)] pending: bool },
    /// The Skills screen's notebooks, answered to the client that asked (Phase 3 §6).
    Skills { notebooks: Vec<Notebook> },
    Busy { job_id: String, text: String },
    State { job: Option<JobState> },
    /// The service could not read a client line. Sent to that client only.
    Error { text: String },
    /// The chat was forgotten (a `clear`): every client empties its screen.
    Cleared {},
}

impl Event {
    /// The job this event belongs to, if any.
    pub fn job_id(&self) -> Option<&str> {
        match self {
            Event::Understood { job_id, .. } | Event::Plan { job_id, .. } | Event::Step { job_id, .. }
            | Event::NeedsAnswer { job_id, .. } | Event::Done { job_id, .. }
            | Event::Failed { job_id, .. } | Event::Stopped { job_id, .. }
            | Event::Learned { job_id, .. } | Event::Busy { job_id, .. } => Some(job_id),
            _ => None,
        }
    }
}

/// A line from a client: `{"say":"…"}` or `{"hello":{}}`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum Request {
    #[serde(rename = "say")] Say(String),
    #[serde(rename = "hello")] Hello(serde_json::Map<String, serde_json::Value>),
    /// The Skills screen asks for the notebooks (Phase 3 §6); answered to that client only.
    #[serde(rename = "skills")] Skills {},
    /// The Skills screen's ✕ on one entry.
    #[serde(rename = "forget")] Forget { notebook: String, topic: String },
    /// Clear: the AI forgets the chat, and every client empties its screen on `Cleared`.
    #[serde(rename = "clear")] Clear {},
}

/// One connection: a writer half and a line-reader half over the same socket. `split` hands the
/// two halves to two threads (a front door types on one and listens on the other); a second
/// connection for that would be a client the service broadcasts to and nobody reads.
pub struct Client { writer: Writer, reader: Reader }
pub struct Writer { stream: UnixStream }
pub struct Reader { lines: BufReader<UnixStream> }

impl Client {
    pub fn connect(socket: &Path) -> std::io::Result<Client> {
        let s = UnixStream::connect(socket)?;
        Ok(Client { reader: Reader { lines: BufReader::new(s.try_clone()?) }, writer: Writer { stream: s } })
    }
    pub fn split(self) -> (Reader, Writer) { (self.reader, self.writer) }
    pub fn say(&mut self, text: &str) -> std::io::Result<()> { self.writer.say(text) }
    pub fn hello(&mut self) -> std::io::Result<()> { self.writer.hello() }
    pub fn next_event(&mut self) -> Option<Event> { self.reader.next_event() }
    /// A read that waits longer than `d` ends the connection (`next_event` gives `None`) instead
    /// of hanging forever; for scripts and tests, never the rail.
    pub fn set_read_timeout(&self, d: Option<std::time::Duration>) -> std::io::Result<()> { self.reader.lines.get_ref().set_read_timeout(d) }
}

impl Writer {
    fn send(&mut self, r: &Request) -> std::io::Result<()> {
        let mut line = serde_json::to_string(r).map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
        line.push('\n');
        self.stream.write_all(line.as_bytes())
    }
    pub fn say(&mut self, text: &str) -> std::io::Result<()> { self.send(&Request::Say(text.to_string())) }
    pub fn hello(&mut self) -> std::io::Result<()> { self.send(&Request::Hello(Default::default())) }
    /// Any request, for a client (the Skills screen) that builds its own.
    pub fn request(&mut self, r: &Request) -> std::io::Result<()> { self.send(r) }
    /// Close the connection in both directions. `split` handed out two fds over one socket
    /// (`try_clone`), so dropping the writer leaves the reader's thread parked on a live socket;
    /// this makes its `read_line` return 0 so it can end. Idempotent enough to call on any exit.
    pub fn shutdown(&self) { let _ = self.stream.shutdown(std::net::Shutdown::Both); }
}

impl Reader {
    /// The next event, or `None` when the service closed the connection. A line that is not an
    /// event is skipped (a newer service may send kinds this client does not know). The
    /// service's extra `seq` key is ignored by serde.
    pub fn next_event(&mut self) -> Option<Event> {
        loop {
            let mut line = String::new();
            match self.lines.read_line(&mut line) {
                Ok(0) | Err(_) => return None,
                Ok(_) => if let Ok(e) = serde_json::from_str::<Event>(&line) { return Some(e); },
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kind_is_the_first_key_of_every_event() {
        let samples = vec![
            Event::Said { text: "hi".into() },
            Event::You { text: "yo".into() },
            Event::Understood { job_id: "j".into(), name: "p".into(), text: "Starting p".into(), housekeeping: false },
            Event::Plan { job_id: "j".into(), steps: vec!["a".into()] },
            Event::Step { job_id: "j".into(), plan_step: 1, text: "wrote a".into(), ok: true },
            Event::NeedsAnswer { job_id: "j".into(), questions: vec!["?".into()], options: vec![vec!["a".into(), "b".into()]] },
            Event::Done { job_id: "j".into(), text: "done".into(), check: Some("ran true".into()), files: vec![ChangedFile { path: "/a".into(), kind: FileKind::Text, size: 1 }], windows: vec![] },
            Event::Failed { job_id: "j".into(), text: "gave up".into(), files: vec![] },
            Event::Stopped { job_id: "j".into(), text: "Stopped".into(), files: vec![] },
            Event::Learned { job_id: "j".into(), lines: vec!["Learned: open a website (this computer)".into()], pending: false },
            Event::Skills { notebooks: vec![Notebook { name: "this computer".into(), entries: vec![NoteView { topic: "t".into(), kind: "technique".into(), text: "x".into(), uses: 2, failed: false, needs_check: true }] }] },
            Event::Busy { job_id: "j".into(), text: "working".into() },
            Event::State { job: None },
            Event::Error { text: "bad".into() },
        ];
        for e in samples {
            let s = serde_json::to_string(&e).unwrap();
            assert!(s.starts_with("{\"kind\":\""), "{s}");
            let back: Event = serde_json::from_str(&s).unwrap();
            assert_eq!(back, e);
        }
    }

    #[test]
    fn skills_requests_have_the_wire_shape_the_service_reads() {
        assert_eq!(serde_json::to_string(&Request::Skills {}).unwrap(), r#"{"skills":{}}"#);
        let f = Request::Forget { notebook: "blender".into(), topic: "bevel".into() };
        assert_eq!(serde_json::from_str::<Request>(&serde_json::to_string(&f).unwrap()).unwrap(), f);
        let e = Event::Skills { notebooks: vec![Notebook { name: "this computer".into(), entries: vec![NoteView { topic: "t".into(), kind: "technique".into(), text: "x".into(), uses: 2, failed: false, needs_check: true }] }] };
        assert!(serde_json::to_string(&e).unwrap().starts_with(r#"{"kind":"skills""#));
    }

    #[test]
    fn state_round_trips_with_waiting() {
        let st = JobState {
            id: "j".into(), name: "p".into(), housekeeping: false, understood: "u".into(),
            plan: vec!["a".into()],
            steps: vec![StepView { plan_step: 1, text: "wrote a".into(), ok: true }],
            waiting: Waiting::Answer { questions: vec!["q".into()], options: vec![] },
        };
        let e = Event::State { job: Some(st.clone()) };
        let back: Event = serde_json::from_str(&serde_json::to_string(&e).unwrap()).unwrap();
        assert_eq!(back, e);
        let none: Waiting = serde_json::from_str("\"none\"").unwrap();
        assert_eq!(none, Waiting::None);
    }

    #[test]
    fn an_old_done_without_windows_still_parses() {
        let old = r#"{"kind":"done","job_id":"j","text":"t","check":null,"files":[]}"#;
        let e: Event = serde_json::from_str(old).unwrap();
        assert!(matches!(&e, Event::Done { windows, .. } if windows.is_empty()));
    }

    #[test]
    fn file_kind_from_extension() {
        assert_eq!(FileKind::of("a.png"), FileKind::Image);
        assert_eq!(FileKind::of("a.JPG"), FileKind::Image);
        assert_eq!(FileKind::of("a.py"), FileKind::Text);
        assert_eq!(FileKind::of("BLUEPRINT.md"), FileKind::Text);
        assert_eq!(FileKind::of("a.bin"), FileKind::Other);
        assert_eq!(FileKind::of("Makefile"), FileKind::Other);
    }
}
