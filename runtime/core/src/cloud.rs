//! The free-cloud pool (parent §4.3; the owner, 2026-09-19): the person's cloud accounts, tried in
//! the order they were added, the next one taking over when one says its allowance is used up.
//! A switch the person turns on and off in the chat window, which stays on until they turn it off
//! (their call). Off, or every account used up: the model on their own machines answers.
//!
//! Both live in files the chat window writes, read on every turn, so the switch and a new account
//! take effect with no restart:
//!   ~/.config/ai-os/cloud-on     exists while the switch is on
//!   ~/.config/ai-os/cloud.tsv    `name<TAB>kind<TAB>url<TAB>model<TAB>key` per account, mode 0600
use crate::find::Kind;
use crate::model::{Model, ModelError, Prompt, RemoteModel};
use crate::moves::Move;
use std::cell::RefCell;
use std::collections::HashMap;
use std::path::PathBuf;
use std::time::{Duration, Instant};

pub const SWITCH: &str = "cloud-on";
pub const ACCOUNTS: &str = "cloud.tsv";
/// How long a used-up account is left alone before it is tried again.
/// ponytail: one rest for every provider; per-provider reset times if a provider's window matters.
pub const REST: Duration = Duration::from_secs(15 * 60);

#[derive(Debug, Clone, PartialEq)]
pub struct Account { pub name: String, pub kind: Kind, pub url: String, pub model: String, pub key: String }

/// `cloud.tsv`'s lines; one that does not read is skipped.
pub fn parse(tsv: &str) -> Vec<Account> {
    tsv.lines().filter_map(|l| {
        let [name, kind, url, model, key] = l.split('\t').collect::<Vec<_>>()[..] else { return None };
        Some(Account { name: name.into(), kind: Kind::parse(kind)?, url: url.into(), model: model.into(), key: key.into() })
    }).collect()
}

/// The folder both files live in.
pub fn dir() -> PathBuf {
    PathBuf::from(std::env::var("HOME").unwrap_or_default()).join(".config/ai-os")
}

/// The machine's own model, with the cloud accounts in front of it while the switch is on.
pub struct Pooled<M: Model> {
    pub local: M,
    pub dir: PathBuf,
    /// Accounts that said they were used up, by `url model`, and when.
    resting: RefCell<HashMap<String, Instant>>,
}

impl<M: Model> Pooled<M> {
    pub fn new(local: M, dir: PathBuf) -> Self { Self { local, dir, resting: RefCell::default() } }
}

impl<M: Model> Model for Pooled<M> {
    fn next_move(&self, prompt: &Prompt) -> Result<Move, ModelError> { self.next_move_watched(prompt, &mut |_| true) }

    fn next_move_watched(&self, prompt: &Prompt, watch: &mut dyn FnMut(usize) -> bool) -> Result<Move, ModelError> {
        if !self.dir.join(SWITCH).exists() { return self.local.next_move_watched(prompt, watch) }
        let accounts = parse(&std::fs::read_to_string(self.dir.join(ACCOUNTS)).unwrap_or_default());
        for a in accounts {
            let id = format!("{} {}", a.url, a.model);
            if self.resting.borrow().get(&id).is_some_and(|t| t.elapsed() < REST) { continue }
            let m = RemoteModel { key: Some(a.key.clone()), ..RemoteModel::at(a.kind, &a.url, &a.model) };
            match m.next_move_watched(prompt, watch) {
                Err(ModelError::Quota(why)) => {
                    eprintln!("cloud: {} {} is used up ({why}); trying the next account", a.name, a.model);
                    self.resting.borrow_mut().insert(id, Instant::now());
                }
                // Any other failure is said, not hidden behind the local model: a cloud that
                // never works must not look like one that does.
                Err(ModelError::Http(e)) => return Err(ModelError::Http(format!("cloud {} {}: {e}", a.name, a.model))),
                answer => return answer,
            }
        }
        eprintln!("cloud: no account has allowance left; using this machine's model");
        self.local.next_move_watched(prompt, watch)
    }

    fn context_tokens(&self) -> usize { self.local.context_tokens() }

    /// A cloud account may not see, so with the Cloud switch on the screen is not offered.
    fn sees(&self) -> bool { !self.dir.join(SWITCH).exists() && self.local.sees() }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::FakeModel;
    use crate::testing::{json_response, serve, temp_root};

    fn said(text: &str) -> String {
        let content = serde_json::json!({ "move": "reply", "text": text }).to_string();
        json_response("200 OK", &serde_json::json!({ "message": { "content": content } }).to_string())
    }
    fn local() -> FakeModel { FakeModel::new(vec![Move::Reply { thought: String::new(), text: "local".into(), outcome: crate::moves::Ending::Done }]) }
    fn prompt() -> Prompt { Prompt { system: "s".into(), user: "u".into(), allowed: vec![], image: None, ..Default::default() } }
    fn text(m: Move) -> String { match m { Move::Reply { text, .. } => text, m => panic!("{m:?}") } }

    #[test]
    fn accounts_read_from_their_lines() {
        let a = parse("Ollama\tollama\thttps://ollama.com\tgpt-oss:120b\tk1\nbroken line\nX\tnope\tu\tm\tk\n");
        assert_eq!(a, vec![Account { name: "Ollama".into(), kind: Kind::Ollama, url: "https://ollama.com".into(), model: "gpt-oss:120b".into(), key: "k1".into() }]);
    }

    #[test]
    fn switched_off_the_cloud_is_never_asked() {
        let d = temp_root("cloud-off");
        let used = serve(said("cloud"), 1);
        std::fs::write(d.join(ACCOUNTS), format!("A\tollama\thttp://{used}\tm\tk\n")).unwrap();
        assert_eq!(text(Pooled::new(local(), d).next_move(&prompt()).unwrap()), "local");
    }

    #[test]
    fn a_used_up_account_hands_over_to_the_next_then_rests() {
        let d = temp_root("cloud-on");
        std::fs::write(d.join(SWITCH), "").unwrap();
        let spent = serve(json_response("429 Too Many Requests", r#"{"error":"you have reached your hourly limit"}"#), 1);
        let fresh = serve(said("second"), 2);
        std::fs::write(d.join(ACCOUNTS), format!("A\tollama\thttp://{spent}\tm\tk\nB\tollama\thttp://{fresh}\tm\tk\n")).unwrap();
        let p = Pooled::new(local(), d);
        assert_eq!(text(p.next_move(&prompt()).unwrap()), "second");
        assert_eq!(text(p.next_move(&prompt()).unwrap()), "second", "the spent account is resting, not asked again (its server would not answer)");
    }

    #[test]
    fn every_account_used_up_falls_back_to_this_machine() {
        let d = temp_root("cloud-spent");
        std::fs::write(d.join(SWITCH), "").unwrap();
        let spent = serve(json_response("402 Payment Required", r#"{"error":"weekly usage limit"}"#), 1);
        std::fs::write(d.join(ACCOUNTS), format!("A\tollama\thttp://{spent}\tm\tk\n")).unwrap();
        assert_eq!(text(Pooled::new(local(), d).next_move(&prompt()).unwrap()), "local");
    }

    #[test]
    fn a_broken_account_is_reported_not_hidden() {
        let d = temp_root("cloud-broken");
        std::fs::write(d.join(SWITCH), "").unwrap();
        let bad = serve(json_response("401 Unauthorized", r#"{"error":"unauthorized"}"#), 1);
        std::fs::write(d.join(ACCOUNTS), format!("A\tollama\thttp://{bad}\tm\tk\n")).unwrap();
        let e = Pooled::new(local(), d).next_move(&prompt()).unwrap_err().to_string();
        assert!(e.contains("cloud A m") && e.contains("unauthorized"), "{e}");
    }
}
