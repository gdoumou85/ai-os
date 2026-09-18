//! The rail opens with the session on the full edition (desktop design §3 step 7, §4): one desktop
//! entry serves the app grid and XDG autostart. `desktop-file-validate` is not in the toolchain, so
//! this reads the file and checks the keys the two uses need.
const ENTRY: &str = include_str!("../org.aios.Rail.desktop");

fn value(key: &str) -> Option<&'static str> {
    ENTRY.lines().find_map(|l| l.strip_prefix(key).and_then(|r| r.strip_prefix('=')))
}

#[test]
fn the_entry_launches_the_rail_and_autostarts() {
    assert_eq!(ENTRY.lines().next(), Some("[Desktop Entry]"));
    assert_eq!(value("Type"), Some("Application"));
    assert_eq!(value("Exec"), Some("ai-os-rail"));
    assert_eq!(value("Name"), Some("AI OS"));
    assert_eq!(value("X-GNOME-Autostart-enabled"), Some("true"));
    assert!(!ENTRY.contains('\r'), "LF only");
}
