// The desktop hand on a real window (2a §12 item 1's live half). Inside the distro with the
// invisible session up:  AI_OS_DESKTOP=1 cargo test -p executor --test desktop_live -- --nocapture
use executor::action::Action;
use executor::atspi::{DesktopState, DesktopWorker};
use executor::desktop::Displays;
use executor::worker::Worker;

#[test]
fn opens_an_editor_looks_types_and_reads_back() {
    if std::env::var("AI_OS_DESKTOP").is_err() { eprintln!("skipped: AI_OS_DESKTOP not set"); return; }
    let w = DesktopWorker(DesktopState::new(40, Displays::from_env()));
    let o = w.run(&Action::OpenApp { name: "org.gnome.TextEditor".into(), visible: false });
    assert!(o.ok, "{}", o.detail);
    let looked = w.run(&Action::Look { window: Some("Text Editor".into()), find: Some("text".into()) });
    assert!(looked.ok && looked.detail.contains("[text]"), "{}", looked.detail);
    let id: u32 = looked.detail.lines().find(|l| l.contains("[text]")).unwrap().split(' ').next().unwrap().parse().unwrap();
    let typed = w.run(&Action::Type { control: id, text: "hello from ai-os".into(), replace: true });
    assert!(typed.ok, "{}", typed.detail);
    let read = w.run(&Action::Read { control: id, from_line: None, lines: None });
    assert!(read.ok && read.detail.contains("hello from ai-os"), "{}", read.detail);
    // The title mirrors the buffer's first line (the 2a probe's proof that the insert committed).
    let again = w.run(&Action::Look { window: None, find: None });
    assert!(again.detail.contains("hello from ai"), "{}", again.detail);
    let closes = w.run(&Action::Look { window: Some("Text Editor".into()), find: Some("close".into()) });
    println!("{}", closes.detail);
    // Press, on the real bus, so `NActions` and `DoAction` are both proven. GTK 4 wraps a menu
    // button in a push button that carries no action of its own, with the real one on the toggle
    // button behind it — so this one look exercises both answers the guard can get.
    let menus = w.run(&Action::Look { window: Some("Text Editor".into()), find: Some("menu".into()) });
    assert!(menus.ok, "{}", menus.detail);
    println!("{}", menus.detail);
    let control = |role: &str| -> (u32, String) {
        let line = menus.detail.lines().find(|l| l.contains(role)).unwrap_or_else(|| panic!("no {role}:\n{}", menus.detail));
        let (id, rest) = line.split_once(' ').unwrap();
        let name = rest.split_once("] ").unwrap().1;
        let name = name.strip_suffix(')').and_then(|s| s.rsplit_once(" (")).map_or(name, |(n, _)| n);
        (id.parse().unwrap(), name.to_string())
    };
    let (wrapper, wrapper_name) = control("[push button]");
    let (id, name) = control("[toggle button]");
    let dead = w.run(&Action::Press { control: wrapper, name: wrapper_name.clone() });
    // The refusal names the one to press instead, or the model guesses (the live 2a run guessed
    // key presses and gave up).
    assert!(!dead.ok && dead.detail == format!("{wrapper_name} has no action to press; the other control named {wrapper_name} is {id} — press that one"), "{}", dead.detail);
    let pressed = w.run(&Action::Press { control: id, name: name.clone() });
    assert!(pressed.ok && pressed.detail == format!("pressed {name}"), "{}", pressed.detail);
    println!("{}", pressed.detail);
    let _ = std::process::Command::new("pkill").args(["-u", "ai", "-f", "gnome-text-editor"]).status();
}
