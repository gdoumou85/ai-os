//! Plain words for actions, and the lines the terminal prints for an event (1d design §2.1).
//! `lines` reproduces, word for word, what `Engine::handle` printed before events existed.
use aios_proto::Event;
use executor::action::{Action, Manager, ServiceDo};

pub fn describe(action: &Action) -> String {
    match action {
        Action::WriteFile { path, .. } => format!("wrote {path}"),
        Action::EditFile { path, .. } => format!("edited {path}"),
        Action::ReadFile { path, .. } => format!("read {path}"),
        Action::RunCommand { argv } => format!("ran {}", argv.join(" ")),
        Action::HttpPost { url, .. } => format!("http post to {url}"),
        Action::Install { packages } => format!("installed {}", packages.join(" ")),
        Action::Remove { packages } => format!("removed {}", packages.join(" ")),
        // The field is `action` in Rust; `do` is only its serde name (action.rs:23).
        Action::Service { name, action } => format!("service {name} {}", match action { ServiceDo::Enable => "enable", ServiceDo::Disable => "disable", ServiceDo::Restart => "restart" }),
        Action::MakeDir { path } => format!("made folder {path}"),
        Action::FetchPackages { manager, packages } => format!("fetched {} with {}", packages.join(" "), match manager { Manager::Pip => "pip", Manager::Npm => "npm", Manager::Cargo => "cargo" }),
        Action::SetSetting { key, value } => format!("set {key} = {value}"),
    }
}

/// The terminal's lines for one event. `plan`, `step` and `state` print nothing: the terminal
/// never showed steps, and the rail is what draws them.
pub fn lines(event: &Event) -> Vec<String> {
    match event {
        Event::Said { text } | Event::Understood { text, .. } | Event::Done { text, .. }
        | Event::Failed { text, .. } | Event::Stopped { text, .. } | Event::Busy { text, .. } => vec![text.clone()],
        Event::You { .. } | Event::Plan { .. } | Event::Step { .. } | Event::State { .. } => vec![],
        Event::NeedsAnswer { questions, .. } => questions.iter().map(|q| format!("Question: {q}")).collect(),
        Event::NeedsOk { why, .. } => vec![format!("Needs your OK: {why}. Say yes to allow it, no to refuse, or ask me about it.")],
        Event::Undone { name, lines, notes, .. } => {
            let mut v = vec![format!("Undoing the last job ({name}):")];
            v.extend(lines.iter().map(|l| l.text.clone()));
            v.extend(notes.iter().cloned());
            v
        }
        Event::Error { text } => vec![format!("(error: {text})")],
    }
}
