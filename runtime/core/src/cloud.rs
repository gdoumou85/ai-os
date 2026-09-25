//! The free-cloud pool (parent §4.3; the owner, 2026-09-19): the person's cloud accounts, tried in
//! the order they were added, the next one taking over when one says its allowance is used up.
//! A switch the person turns on and off in the chat window, which stays on until they turn it off
//! (their call). Off, or every account used up: the model on their own machines answers.
//!
//! Both live in files the chat window writes, read on every turn, so the switch and a new account
//! take effect with no restart:
//!   ~/.config/ai-os/cloud-on     exists while the switch is on
//!   ~/.config/ai-os/cloud.tsv    `name<TAB>kind<TAB>url<TAB>model<TAB>key` per account, mode 0600
use crate::find::{self, Kind};
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
    /// The chat models an account's key opens, as its provider lists them.
    pub others: Box<dyn Fn(&Account) -> Vec<String>>,
    /// A switch to another model the owner has not been told about yet.
    news: RefCell<Option<String>>,
}

impl<M: Model> Pooled<M> {
    pub fn new(local: M, dir: PathBuf) -> Self {
        let others = Box::new(|a: &Account| aios_proto::chat_models(&a.url, find::ask(a.kind, &a.url, Some(&a.key)).into_iter().filter_map(|f| f.model).collect()));
        Self { local, dir, resting: RefCell::default(), others, news: RefCell::default() }
    }

    fn resting(&self, url: &str, model: &str) -> bool {
        self.resting.borrow().get(&format!("{url} {model}")).is_some_and(|t| t.elapsed() < REST)
    }

    /// One account's answer from `model`; a used-up model is left to rest.
    fn ask(&self, a: &Account, model: &str, prompt: &Prompt, watch: &mut dyn FnMut(usize) -> bool) -> Result<Move, ModelError> {
        let m = RemoteModel { key: Some(a.key.clone()), ..RemoteModel::at(a.kind, &a.url, model) };
        let answer = m.next_move_watched(&crate::model::format_spelled(prompt), watch);
        if let Err(ModelError::Quota(why)) = &answer {
            eprintln!("cloud: {} {model} is used up ({why})", a.name);
            self.resting.borrow_mut().insert(format!("{} {model}", a.url), Instant::now());
        }
        answer
    }

    /// The account now uses `model`, in the file the Cloud card shows too.
    fn keep(&self, a: &Account, model: &str) {
        let path = self.dir.join(ACCOUNTS);
        let tsv = std::fs::read_to_string(&path).unwrap_or_default();
        let kept: String = tsv.lines().map(|l| if parse(l).first() == Some(a) {
            format!("{}\t{}\t{}\t{model}\t{}\n", a.name, a.kind.as_str(), a.url, a.key)
        } else { format!("{l}\n") }).collect();
        // Rewritten in place, so the file keeps its 0600.
        if let Err(e) = std::fs::write(&path, kept) { eprintln!("cloud: could not keep {model}: {e}") }
    }
}

impl<M: Model> Model for Pooled<M> {
    fn next_move(&self, prompt: &Prompt) -> Result<Move, ModelError> { self.next_move_watched(prompt, &mut |_| true) }

    fn next_move_watched(&self, prompt: &Prompt, watch: &mut dyn FnMut(usize) -> bool) -> Result<Move, ModelError> {
        if !self.dir.join(SWITCH).exists() { return self.local.next_move_watched(prompt, watch) }
        let accounts = parse(&std::fs::read_to_string(self.dir.join(ACCOUNTS)).unwrap_or_default());
        for a in accounts {
            if self.resting(&a.url, &a.model) { continue }
            let failed = match self.ask(&a, &a.model, prompt, watch) {
                Err(e @ (ModelError::Quota(_) | ModelError::Http(_))) => e,
                answer => return answer,
            };
            // The picked model failed: another the same key opens takes over, and stays picked
            // (the owner, 2026-09-25). ponytail: three tries, so a bad key never walks a whole list.
            let others: Vec<String> = (self.others)(&a).into_iter().filter(|m| *m != a.model && !self.resting(&a.url, m)).take(3).collect();
            for m in others {
                match self.ask(&a, &m, prompt, watch) {
                    Err(ModelError::Quota(_) | ModelError::Http(_)) => continue,
                    Err(ModelError::Stopped) => return Err(ModelError::Stopped),
                    answer => {
                        self.keep(&a, &m);
                        *self.news.borrow_mut() = Some(format!("(Cloud: {} {} did not answer, so I switched to {m}. Model → Cloud accounts changes it.)", a.name, a.model));
                        return answer
                    }
                }
            }
            match failed {
                ModelError::Quota(_) => eprintln!("cloud: {} has no model with allowance left; trying the next account", a.name),
                // Any other failure is said, not hidden behind the local model: a cloud that
                // never works must not look like one that does.
                e => return Err(ModelError::Http(format!("cloud {} {}: {}", a.name, a.model, match e { ModelError::Http(w) => w, e => e.to_string() }))),
            }
        }
        eprintln!("cloud: no account has allowance left; using this machine's model");
        self.local.next_move_watched(prompt, watch)
    }

    fn context_tokens(&self) -> usize { self.local.context_tokens() }

    fn news(&self) -> Option<String> { self.news.borrow_mut().take() }

    /// A cloud account may not see, so with the Cloud switch on the screen is not offered.
    fn sees(&self) -> bool { !self.dir.join(SWITCH).exists() && self.local.sees() }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::FakeModel;
    use crate::testing::{json_response, serve, serve_each, temp_root};

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
    fn a_failing_model_hands_over_to_another_the_key_opens_and_it_stays() {
        let d = temp_root("cloud-switch");
        std::fs::write(d.join(SWITCH), "").unwrap();
        let refused = json_response("400 Bad Request", r#"{"error":"max_tokens (131072) exceeds model's maximum output tokens (65536)"}"#);
        let at = serve_each(vec![refused, said("switched"), said("again")]);
        std::fs::write(d.join(ACCOUNTS), format!("A\tollama\thttp://{at}\tbig\tk\n")).unwrap();
        let mut p = Pooled::new(local(), d.clone());
        p.others = Box::new(|_| vec!["big".into(), "other".into()]);
        assert_eq!(text(p.next_move(&prompt()).unwrap()), "switched");
        assert!(p.news().is_some_and(|n| n.contains("big") && n.contains("other")), "the owner hears of the switch");
        assert_eq!(p.news(), None, "once");
        assert_eq!(std::fs::read_to_string(d.join(ACCOUNTS)).unwrap(), format!("A\tollama\thttp://{at}\tother\tk\n"));
        assert_eq!(text(p.next_move(&prompt()).unwrap()), "again", "the new model is asked straight away next time");
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
