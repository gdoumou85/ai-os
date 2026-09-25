//! Plain words for actions, and the lines the terminal prints for an event (1d design §2.1).
//! `lines` reproduces, word for word, what `Engine::handle` printed before events existed.
use aios_proto::Event;
use executor::action::Action;

pub fn describe(action: &Action) -> String {
    match action {
        Action::WriteFile { path, .. } => format!("wrote {path}"),
        Action::EditFile { path, .. } => format!("edited {path}"),
        Action::AppendFile { path, .. } => format!("added to {path}"),
        Action::ReadFile { path, .. } => format!("read {path}"),
        Action::RunCommand { argv } => format!("ran {}", argv.join(" ")),
        Action::SetSetting { key, value } => format!("set {key} = {value}"),
        // A blank name is no window — the hand reads it as none — so the card must not say
        // "looked at ", as the live 2a run's own Done check line did.
        Action::Look { window, find } => match (window.as_deref().map(str::trim).filter(|w| !w.is_empty()), find) {
            (None, _) => "looked at the open windows".into(),
            (Some(w), None) => format!("looked at {w}"),
            (Some(w), Some(f)) => format!("looked for {f} in {w}"),
        },
        Action::Press { name, .. } => format!("pressed {name}"),
        Action::Type { text, control, .. } => format!("typed {} into control {control}", plural(text.lines().count().max(1), "line")),
        Action::Read { control, .. } => format!("read control {control}"),
        Action::OpenApp { name, .. } => format!("opened {name}"),
        // The card is the notice that the AI has the screen, so it says how to take it back.
        Action::ScreenLook { cell: None } => "looked at the screen (move the mouse to stop me)".into(),
        Action::ScreenLook { cell: Some(c) } => format!("looked closely at screen square {c}"),
        Action::ScreenClick { name, double, .. } => format!("{} {name} on the screen", if *double { "double-clicked" } else { "clicked" }),
        Action::ScreenType { text, enter } => format!("typed {} on the screen{}", plural(text.chars().count(), "character"), if *enter { " and pressed Enter" } else { "" }),
        Action::WebRead { url, .. } => format!("read the page {url}"),
        Action::WebSearch { query } => format!("searched the web for {query}"),
        Action::StartProgram { name, argv } => format!("started {name} ({}) in the background", argv.join(" ")),
        Action::ProgramOutput { name, .. } => format!("checked on {name}"),
        Action::StopProgram { name } => format!("stopped {name}"),
        Action::Key { keys } => format!("pressed {keys}"),
        Action::Scroll { direction, .. } => format!("scrolled {direction}"),
        Action::Drag { from_cell, to_cell, .. } => format!("dragged from square {from_cell} to square {to_cell}"),
        Action::Wait { seconds } => format!("waited {seconds} s"),
        Action::Watch { name, when, .. } => format!("set the watcher {name} ({when})"),
        Action::Unwatch { name } => format!("removed the watcher {name}"),
        Action::Delegate { helpers } => format!("handed work to {} helper{}", helpers.len(), if helpers.len() == 1 { "" } else { "s" }),
    }
}

/// The same actions in the present tense, for the line that shows *while* one runs (the
/// owner, 2026-09-20: "the local LLM keeps working and i do not know what it does"). `describe`
/// is the past tense that lands on the card once the action is over.
pub fn doing(action: &Action) -> String {
    match action {
        Action::WriteFile { path, .. } => format!("writing {path}"),
        Action::EditFile { path, .. } => format!("editing {path}"),
        Action::AppendFile { path, .. } => format!("adding to {path}"),
        Action::ReadFile { path, .. } => format!("reading {path}"),
        Action::RunCommand { argv } => format!("running {}", argv.join(" ")),
        Action::SetSetting { key, value } => format!("setting {key} = {value}"),
        Action::Look { window, .. } => match window.as_deref().map(str::trim).filter(|w| !w.is_empty()) {
            None => "looking at the open windows".into(),
            Some(w) => format!("looking at {w}"),
        },
        Action::Press { name, .. } => format!("pressing {name}"),
        Action::Type { control, .. } => format!("typing into control {control}"),
        Action::Read { control, .. } => format!("reading control {control}"),
        Action::OpenApp { name, .. } => format!("opening {name}"),
        Action::ScreenLook { cell: None } => "looking at the screen".into(),
        Action::ScreenLook { cell: Some(c) } => format!("looking closely at screen square {c}"),
        Action::ScreenClick { name, .. } => format!("clicking {name} on the screen"),
        Action::ScreenType { .. } => "typing on the screen".into(),
        Action::WebRead { url, .. } => format!("reading the page {url}"),
        Action::WebSearch { query } => format!("searching the web for {query}"),
        Action::StartProgram { name, .. } => format!("starting {name}"),
        Action::ProgramOutput { name, .. } => format!("checking on {name}"),
        Action::StopProgram { name } => format!("stopping {name}"),
        Action::Key { keys } => format!("pressing {keys}"),
        Action::Scroll { direction, .. } => format!("scrolling {direction}"),
        Action::Drag { .. } => "dragging".into(),
        Action::Wait { seconds } => format!("waiting {seconds} s"),
        Action::Watch { name, .. } => format!("setting the watcher {name}"),
        Action::Unwatch { name } => format!("removing the watcher {name}"),
        Action::Delegate { helpers } => format!("{} helper{} working", helpers.len(), if helpers.len() == 1 { "" } else { "s" }),
    }
}

fn plural(n: usize, w: &str) -> String { if n == 1 { format!("1 {w}") } else { format!("{n} {w}s") } }

/// The terminal's lines for one event. `plan`, `step` and `state` print nothing: the terminal
/// never showed steps, and the rail is what draws them.
pub fn lines(event: &Event) -> Vec<String> {
    match event {
        Event::Said { text } | Event::Understood { text, .. }
        | Event::Done { text, .. } | Event::Failed { text, .. } | Event::Stopped { text, .. } | Event::Busy { text, .. } => vec![text.clone()],
        Event::You { .. } | Event::Plan { .. } | Event::Step { .. } | Event::State { .. } | Event::Skills { .. } | Event::Projects { .. } | Event::Watchers { .. } | Event::Cleared {} => vec![],
        Event::NeedsAnswer { questions, options, .. } => questions.iter().enumerate().map(|(i, q)| match options.get(i).filter(|o| !o.is_empty()) {
            Some(o) => format!("Question: {q} ({})", o.join(" / ")),
            None => format!("Question: {q}"),
        }).collect(),
        Event::Learned { lines, pending, .. } => {
            let mut v = lines.clone();
            if *pending { v.push("Say \"keep what you learned\" to keep what changes the machine, or \"discard what you learned\".".into()); }
            v
        }
        Event::Error { text } => vec![format!("(error: {text})")],
        Event::Alert { watcher, text, .. } => vec![format!("Alert from {watcher}: {text}")],
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
        assert_eq!(describe(&Action::Look { window: None, find: None }), "looked at the open windows");
        // The 9B writes `window: ""` for "list the windows" and the hand obliges, so the card
        // must read like the one above, not "looked at ".
        assert_eq!(describe(&Action::Look { window: Some(String::new()), find: None }), "looked at the open windows");
        assert_eq!(describe(&Action::Look { window: Some("  ".into()), find: Some("save".into()) }), "looked at the open windows");
        assert_eq!(describe(&Action::Look { window: Some("Calculator".into()), find: Some("save".into()) }), "looked for save in Calculator");
        assert_eq!(describe(&Action::OpenApp { name: "org.gnome.Calculator".into(), visible: false }), "opened org.gnome.Calculator");
    }

    #[test]
    fn what_it_is_doing_reads_in_the_present_tense() {
        assert_eq!(doing(&Action::RunCommand { argv: vec!["sudo".into(), "apt-get".into(), "install".into(), "blender".into()] }), "running sudo apt-get install blender");
        assert_eq!(doing(&Action::WriteFile { path: "a.py".into(), contents: "x".into() }), "writing a.py");
        assert_eq!(doing(&Action::OpenApp { name: "firefox".into(), visible: true }), "opening firefox");
        assert_eq!(doing(&Action::Look { window: Some(String::new()), find: None }), "looking at the open windows");
    }

    #[test]
    fn the_terminal_prints_a_done_as_its_own_words() {
        let e = Event::Done { job_id: "j".into(), text: "done".into(), check: None, files: vec![], windows: vec!["Calculator".into()] };
        assert_eq!(lines(&e), vec!["done".to_string()], "no undo, so no note about what cannot be undone");
    }
}
