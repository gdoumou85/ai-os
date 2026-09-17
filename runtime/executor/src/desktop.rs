//! The desktop hand's pure half (2a design §3–§4): what a window's tree becomes for the model,
//! and the id table. No bus here; `atspi.rs` fills `Node`s from the real one.

/// One accessible object, as much of it as the hand reads.
#[derive(Debug, Clone, PartialEq)]
pub struct Node {
    pub app: String,
    pub path: String,
    pub role: String,
    pub name: String,
    pub showing: bool,
    pub sensitive: bool,
    pub editable: bool,
    pub checked: bool,
    pub focused: bool,
    /// The first 60 characters of a text control's contents, when it has any.
    pub text: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Entry { pub app: String, pub path: String, pub name: String }

/// Short ids for the model, one per object for the life of the process; never reused, never
/// cleared (2a §3: an id is only ever the object it was handed out for).
#[derive(Debug, Default)]
pub struct IdTable { entries: Vec<Entry> }

impl IdTable {
    pub fn new() -> Self { Self::default() }
    pub fn id_for(&mut self, n: &Node) -> u32 {
        if let Some(i) = self.entries.iter().position(|e| e.app == n.app && e.path == n.path) {
            self.entries[i].name = n.name.clone();
            return i as u32 + 1;
        }
        self.entries.push(Entry { app: n.app.clone(), path: n.path.clone(), name: n.name.clone() });
        self.entries.len() as u32
    }
    pub fn get(&self, id: u32) -> Option<&Entry> { id.checked_sub(1).and_then(|i| self.entries.get(i as usize)) }
    /// Every id handed out under this name, oldest first. GTK 4 lists one button twice — a
    /// wrapper with no action and the real one behind it — so a refused press can say which.
    pub fn named<'a>(&'a self, name: &'a str) -> impl Iterator<Item = (u32, &'a Entry)> + 'a {
        self.entries.iter().enumerate().filter(move |(_, e)| e.name == name).map(|(i, e)| (i as u32 + 1, e))
    }
}

/// The context budget divided by 200, never below 20 (2a §4): 40 on the 8k workshop.
pub fn look_cap(context_tokens: usize) -> usize { (context_tokens / 200).max(20) }

/// The roles the model can do something with or learn something from.
const INTERESTING: [&str; 21] = [
    "push button", "toggle button", "button", "check box", "radio button", "menu", "menu item", "check menu item",
    "radio menu item", "entry", "password text", "text", "paragraph", "document text", "combo box", "page tab",
    "label", "spin button", "slider", "link", "list item",
];

/// Interactive and readable controls only, showing only, the focused one first, then tree order;
/// `find` keeps those whose name or role contains it (case-insensitive). Long labels are furniture.
pub fn select<'a>(nodes: &'a [Node], find: Option<&str>) -> Vec<&'a Node> {
    let f = find.map(|s| s.to_lowercase());
    // A toolkit commonly exposes a control's own caption as a label beside it: GNOME Calculator
    // lists every one of its keys twice, so half the look is lines that say what the line above
    // already said and carry no action. That is filler like a panel (§4), and on the workshop's
    // 40-control cap it was the difference between seeing the `=` key and never reaching it.
    // ponytail: O(n²) over one window's nodes, thousands at the very most.
    let echoes_a_control = |name: &str| nodes.iter().any(|n| n.showing && n.role != "label" && !name.is_empty()
        && INTERESTING.contains(&n.role.as_str()) && n.name == name);
    let v: Vec<&'a Node> = nodes.iter().filter(|n| n.showing && INTERESTING.contains(&n.role.as_str()))
        .filter(|n| n.role != "label" || (n.name.chars().count() <= 80 && !echoes_a_control(&n.name)))
        .filter(|n| f.as_ref().map_or(true, |f| n.name.to_lowercase().contains(f) || n.role.to_lowercase().contains(f)))
        .collect();
    // ponytail: stable partition keeps tree order within each half.
    let (focused, rest): (Vec<&'a Node>, Vec<&'a Node>) = v.into_iter().partition(|n| n.focused);
    focused.into_iter().chain(rest).collect()
}

/// The text the model reads for `look` on a window.
pub fn render_look(window: &str, chosen: &[&Node], ids: &mut IdTable, cap: usize) -> String {
    let mut out = format!("controls of {window}:\n");
    for n in chosen.iter().take(cap) {
        let id = ids.id_for(n);
        let mut flags = vec![];
        if n.checked { flags.push("checked"); }
        if n.focused { flags.push("focused"); }
        if n.editable { flags.push("editable"); }
        if !n.sensitive { flags.push("disabled"); }
        let flags = if flags.is_empty() { String::new() } else { format!(" ({})", flags.join(", ")) };
        match &n.text {
            Some(t) if n.name.is_empty() => out.push_str(&format!("{id} [{}] {:?}{flags}\n", n.role, t)),
            Some(t) => out.push_str(&format!("{id} [{}] {} {:?}{flags}\n", n.role, n.name, t)),
            None => out.push_str(&format!("{id} [{}] {}{flags}\n", n.role, n.name)),
        }
    }
    if chosen.len() > cap { out.push_str(&format!("{} more, narrow with find\n", chosen.len() - cap)); }
    out
}

/// What `read` gives back: the control's text windowed by line, then capped the way `read_file`
/// caps an un-windowed read (§4 says `read` is "windowed like `read_file` and capped the same
/// way"). A text view holds a whole document, and 200 of its lines are no bound at all on a file
/// of long ones — without the cap one `read` can fill the model's context by itself. The header
/// says when it was cut, so the model knows to ask for the rest by line.
pub fn read_window(all: &str, from_line: Option<usize>, lines: Option<usize>) -> String {
    let v: Vec<&str> = all.lines().collect();
    let from = from_line.unwrap_or(1).max(1);
    let n = lines.unwrap_or(200).min(200);
    let slice: Vec<&str> = v.iter().skip(from - 1).take(n).copied().collect();
    let body = slice.join("\n");
    let shown = crate::worker::head(&body, READ_CAP);
    let cut = if shown.len() < body.len() { format!(", cut at {READ_CAP} characters") } else { String::new() };
    format!("(lines {}-{} of {}{cut})\n{shown}", from, from + slice.len().saturating_sub(1), v.len())
}

/// The same 2000 characters `worker::window` gives an un-windowed `read_file`.
const READ_CAP: usize = 2000;

/// Where `open_app` puts a window (2a §5): the invisible session unless the user asked to see it.
#[derive(Debug, Clone, PartialEq)]
pub struct Displays { pub invisible: String, pub visible: String }

impl Displays {
    pub fn from_env() -> Self {
        Self {
            invisible: std::env::var("AI_OS_DISPLAY_INVISIBLE").unwrap_or_else(|_| "wayland-ai".into()),
            visible: std::env::var("AI_OS_DISPLAY_VISIBLE").unwrap_or_else(|_| "wayland-0".into()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn node(role: &str, name: &str, showing: bool) -> Node {
        Node { app: "app".into(), path: format!("/o/{role}/{name}"), role: role.into(), name: name.into(), showing, sensitive: true, editable: false, checked: false, focused: false, text: None }
    }

    /// §4: `read` is windowed like `read_file` and capped the same way. Without the cap, 200
    /// lines of a real document is no bound — a text view holds the whole file, and one `read`
    /// of a long-lined one fills the model's context by itself.
    #[test]
    fn a_read_is_windowed_by_line_and_then_capped_like_read_file() {
        let long: String = (1..=300).map(|i| format!("line {i} {}\n", "x".repeat(80))).collect();
        let all = read_window(&long, None, None);
        let (header, body) = all.split_once('\n').unwrap();
        assert_eq!(header, "(lines 1-200 of 300, cut at 2000 characters)", "the header says it was cut");
        assert_eq!(body.chars().count(), 2000, "and it was");
        assert!(body.starts_with("line 1 xxx"), "cut from the end, not the start");
        // A window that fits says nothing about a cap, and the header is the one it always had.
        let short = read_window("alpha\nbeta\ngamma\ndelta", Some(2), Some(2));
        assert_eq!(short, "(lines 2-3 of 4)\nbeta\ngamma");
        assert_eq!(read_window("one\ntwo", None, None), "(lines 1-2 of 2)\none\ntwo");
        // Asking past the end is not an error, and `n` is still bounded at 200 lines.
        assert_eq!(read_window("one\ntwo", Some(9), None), "(lines 9-9 of 2)\n");
        assert_eq!(read_window(&long, Some(1), Some(9999)), all, "lines is capped at 200 either way");
    }

    #[test]
    fn the_cap_follows_the_context_and_never_drops_below_twenty() {
        assert_eq!(look_cap(8192), 40);
        assert_eq!(look_cap(32768), 163);
        assert_eq!(look_cap(1000), 20);
    }

    #[test]
    fn select_keeps_controls_drops_furniture_and_puts_the_focused_one_first() {
        let mut save = node("push button", "Save", true); save.focused = true;
        let nodes = vec![node("panel", "", true), node("filler", "", true), node("push button", "Bold", true),
                         node("menu item", "Paste", false), node("label", "1 word, 17 characters", true),
                         node("label", &"x".repeat(200), true), save.clone(), node("text", "", true)];
        let got: Vec<&str> = select(&nodes, None).iter().map(|n| n.name.as_str()).collect();
        assert_eq!(got, vec!["Save", "Bold", "1 word, 17 characters", ""], "focused first, then tree order; hidden menu item and long label dropped");
        let found: Vec<&str> = select(&nodes, Some("bold")).iter().map(|n| n.name.as_str()).collect();
        assert_eq!(found, vec!["Bold"]);
        let by_role: Vec<&str> = select(&nodes, Some("text")).iter().map(|n| n.role.as_str()).collect();
        assert_eq!(by_role, vec!["text"], "find matches the role too");
    }

    /// GNOME Calculator lists all twenty-five of its keys twice — `[push button] =` and
    /// `[label] = "="` — so the 40-control cap cut the look off at `×` and the live run's model
    /// never saw the `=` key at all. A label that only says what a control beside it says is
    /// filler; a label that says something of its own stays.
    #[test]
    fn a_label_that_only_echoes_a_control_is_filler() {
        let nodes = vec![node("push button", "=", true), node("label", "=", true),
                         node("push button", "4", true), node("label", "4", true),
                         node("label", "1 word, 17 characters", true),
                         node("text", "", true), node("label", "", true)];
        let got: Vec<(&str, &str)> = select(&nodes, None).iter().map(|n| (n.role.as_str(), n.name.as_str())).collect();
        assert_eq!(got, vec![("push button", "="), ("push button", "4"), ("label", "1 word, 17 characters"), ("text", ""), ("label", "")],
                   "the two echoing labels go; the status label and the nameless ones stay");
    }

    #[test]
    fn ids_are_stable_per_object_and_never_reused() {
        let mut t = IdTable::new();
        let a = node("push button", "Bold", true);
        let b = node("push button", "Save", true);
        assert_eq!(t.id_for(&a), 1);
        assert_eq!(t.id_for(&b), 2);
        assert_eq!(t.id_for(&a), 1, "same object, same id");
        assert_eq!(t.get(2).unwrap().name, "Save");
        assert!(t.get(3).is_none());
        let mut twin = node("toggle button", "Save", true); twin.path = "/o/other".into();
        assert_eq!(t.id_for(&twin), 3);
        assert_eq!(t.named("Save").map(|(id, _)| id).collect::<Vec<_>>(), vec![2, 3], "both controls of that name");
        assert_eq!(t.named("Nothing").count(), 0);
    }

    #[test]
    fn render_caps_and_says_how_many_more() {
        let mut t = IdTable::new();
        let nodes: Vec<Node> = (0..5).map(|i| node("push button", &format!("B{i}"), true)).collect();
        let chosen: Vec<&Node> = nodes.iter().collect();
        let s = render_look("Editor", &chosen, &mut t, 3);
        assert!(s.starts_with("controls of Editor:\n1 [push button] B0\n2 [push button] B1\n3 [push button] B2\n"), "{s}");
        assert!(s.ends_with("2 more, narrow with find\n"), "{s}");
        let mut checked = node("toggle button", "Bold", true); checked.checked = true; checked.focused = true;
        let mut text = node("text", "", true); text.text = Some("hello world".into()); text.editable = true;
        let s2 = render_look("Editor", &[&checked, &text], &mut t, 10);
        assert!(s2.contains("[toggle button] Bold (checked, focused)"), "{s2}");
        assert!(s2.contains(r#"[text] "hello world" (editable)"#), "{s2}");
    }
}
