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
        Action::Look { window: None, .. } => "looked at the open windows".into(),
        Action::Look { window: Some(w), find: None } => format!("looked at {w}"),
        Action::Look { window: Some(w), find: Some(f) } => format!("looked for {f} in {w}"),
        Action::Press { name, .. } => format!("pressed {name}"),
        Action::Type { text, control, .. } => format!("typed {} into control {control}", plural(text.lines().count().max(1), "line")),
        Action::Read { control, .. } => format!("read control {control}"),
        Action::OpenApp { name, .. } => format!("opened {name}"),
    }
}

fn plural(n: usize, w: &str) -> String { if n == 1 { format!("1 {w}") } else { format!("{n} {w}s") } }

/// The terminal's lines for one event. `plan`, `step` and `state` print nothing: the terminal
/// never showed steps, and the rail is what draws them.
pub fn lines(event: &Event) -> Vec<String> {
    match event {
        Event::Said { text } | Event::Understood { text, .. }
        | Event::Failed { text, .. } | Event::Stopped { text, .. } | Event::Busy { text, .. } => vec![text.clone()],
        Event::Done { text, windows, .. } => { let mut v = vec![text.clone()]; v.extend(aios_proto::window_note(windows)); v }
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

#[cfg(test)]
mod tests {
    use super::*;
    use executor::action::Action;

    #[test]
    fn desktop_steps_read_as_plain_words() {
        assert_eq!(describe(&Action::Press { control: 4, name: "Bold".into() }), "pressed Bold");
        assert_eq!(describe(&Action::Type { control: 2, text: "a\nb".into(), replace: false }), "typed 2 lines into control 2");
        assert_eq!(describe(&Action::Look { window: Some("Text Editor".into()), find: None }), "looked at Text Editor");
        assert_eq!(describe(&Action::OpenApp { name: "org.gnome.Calculator".into(), visible: false }), "opened org.gnome.Calculator");
    }

    #[test]
    fn the_terminal_prints_the_undo_note_after_a_window_job() {
        let e = Event::Done { job_id: "j".into(), text: "done".into(), check: None, files: vec![], windows: vec!["Calculator".into()] };
        assert_eq!(lines(&e), vec!["done".to_string(), "What I did inside Calculator can't be undone by me.".to_string()]);
        let plain = Event::Done { job_id: "j".into(), text: "done".into(), check: None, files: vec![], windows: vec![] };
        assert_eq!(lines(&plain), vec!["done".to_string()], "no windows, no note: 1d's lines unchanged");
    }
}
