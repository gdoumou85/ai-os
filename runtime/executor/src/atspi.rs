//! The desktop hand's bus half (2a design §5): four AT-SPI interfaces over zbus's blocking API,
//! one shared state the engine's worker factory captures, and the worker that performs the five
//! actions. No app is named here.
use crate::action::{valid_app_name, Action};
use crate::desktop::{look_cap, render_look, select, Displays, IdTable, Node};
use crate::worker::{Outcome, Worker};
use std::cell::RefCell;
use std::rc::Rc;
use std::time::{Duration, Instant};
use zbus::blocking::Connection;
use zbus::proxy;
use zbus::zvariant::OwnedObjectPath;

type Ref = (String, OwnedObjectPath);

// `gen_async = false` means the generated blocking type carries no `Blocking` suffix (zbus 5
// only adds one when both proxies are generated), so these are `AccessibleProxy` and friends.
#[proxy(interface = "org.a11y.atspi.Accessible", gen_async = false)]
trait Accessible {
    #[zbus(property)] fn name(&self) -> zbus::Result<String>;
    fn get_role(&self) -> zbus::Result<u32>;
    fn get_state(&self) -> zbus::Result<Vec<u32>>;
    fn get_children(&self) -> zbus::Result<Vec<Ref>>;
}

#[proxy(interface = "org.a11y.atspi.Action", gen_async = false)]
trait ActionIface {
    /// AT-SPI carries the action count as a property, not a `GetNActions` method, and the
    /// generated name would be `Nactions`, so the D-Bus name is pinned here.
    #[zbus(property, name = "NActions")] fn nactions(&self) -> zbus::Result<i32>;
    fn do_action(&self, index: i32) -> zbus::Result<bool>;
}

#[proxy(interface = "org.a11y.atspi.Text", gen_async = false)]
trait Text {
    #[zbus(property)] fn character_count(&self) -> zbus::Result<i32>;
    fn get_text(&self, start: i32, end: i32) -> zbus::Result<String>;
}

#[proxy(interface = "org.a11y.atspi.EditableText", gen_async = false)]
trait EditableText {
    fn insert_text(&self, position: i32, text: &str, length: i32) -> zbus::Result<bool>;
    fn delete_text(&self, start: i32, end: i32) -> zbus::Result<bool>;
}

// AT-SPI StateType bit numbers, read off the enum's order in atspi-constants.h.
const CHECKED: u32 = 4;
const EDITABLE: u32 = 7;
const FOCUSED: u32 = 12;
const SENSITIVE: u32 = 24;
const SHOWING: u32 = 25;
fn has(state: &[u32], bit: u32) -> bool { state.get((bit / 32) as usize).is_some_and(|w| w & (1 << (bit % 32)) != 0) }

/// The role, named the way the design and `desktop::select` name it. The wire's `GetRoleName`
/// is the toolkit's own word for the role — GTK 4 answers "text box", "generic", "group" where
/// AT-SPI's own vocabulary says "text", "panel", "grouping" — so the number is what we ask for
/// and this is the one place it becomes a word. Every role the hand does not act on is "other":
/// `select` drops it, so the model never reads it. Numbers from atspi-constants.h's role enum.
fn role_name(role: u32) -> &'static str {
    match role {
        7 => "check box",
        8 => "check menu item",
        11 => "combo box",
        29 => "label",
        32 => "list item",
        33 => "menu",
        35 => "menu item",
        37 => "page tab",
        40 => "password text",
        43 => "push button",
        44 => "radio button",
        45 => "radio menu item",
        51 => "slider",
        52 => "spin button",
        61 => "text",
        62 => "toggle button",
        73 => "paragraph",
        79 => "entry",
        88 => "link",
        94 => "document text",
        _ => "other",
    }
}

const REGISTRY: &str = "org.a11y.atspi.Registry";
const ROOT: &str = "/org/a11y/atspi/accessible/root";
/// The walk's node bound, as the Phase 0 probe's. ponytail: one round-trip per node; the
/// `org.a11y.atspi.Cache` interface (all nodes of an app in one call) is the upgrade if a big
/// window's look takes too long.
const MAX_NODES: usize = 6000;

/// Shared by every `DesktopWorker` the factory hands out: the connection opens on first use,
/// the id table lives as long as the process.
pub struct DesktopState { conn: Option<Connection>, pub ids: IdTable, cap: usize, displays: Displays }

impl DesktopState {
    pub fn new(cap: usize, displays: Displays) -> Rc<RefCell<Self>> {
        Rc::new(RefCell::new(Self { conn: None, ids: IdTable::new(), cap, displays }))
    }
    pub fn for_model(context_tokens: usize) -> Rc<RefCell<Self>> { Self::new(look_cap(context_tokens), Displays::from_env()) }

    /// The accessibility bus, found through the session bus's `org.a11y.Bus.GetAddress`.
    fn conn(&mut self) -> Result<&Connection, String> {
        if self.conn.is_none() {
            let session = Connection::session().map_err(|e| format!("no session bus: {e}"))?;
            let reply = session.call_method(Some("org.a11y.Bus"), "/org/a11y/bus", Some("org.a11y.Bus"), "GetAddress", &())
                .map_err(|e| format!("no accessibility bus: {e}"))?;
            let addr: String = reply.body().deserialize().map_err(|e| format!("bad a11y address: {e}"))?;
            let conn = zbus::blocking::connection::Builder::address(addr.as_str()).and_then(|b| b.build())
                .map_err(|e| format!("cannot join the accessibility bus: {e}"))?;
            self.conn = Some(conn);
        }
        Ok(self.conn.as_ref().unwrap())
    }
}

fn acc(conn: &Connection, r: &Ref) -> zbus::Result<AccessibleProxy<'static>> {
    AccessibleProxy::builder(conn).destination(r.0.clone())?.path(r.1.clone())?
        .cache_properties(zbus::proxy::CacheProperties::No).build()
}

fn text_at(conn: &Connection, r: &Ref) -> zbus::Result<TextProxy<'static>> {
    TextProxy::builder(conn).destination(r.0.clone())?.path(r.1.clone())?
        .cache_properties(zbus::proxy::CacheProperties::No).build()
}

/// One node, read through the bus. `None` if the object does not answer (gone).
fn read_node(conn: &Connection, r: &Ref, with_text: bool) -> Option<Node> {
    let a = acc(conn, r).ok()?;
    let role = role_name(a.get_role().ok()?).to_string();
    let state = a.get_state().ok()?;
    let name = a.name().unwrap_or_default();
    let editable = has(&state, EDITABLE);
    let text = if with_text && matches!(role.as_str(), "text" | "entry" | "paragraph" | "document text" | "label") {
        text_at(conn, r).ok().and_then(|t| t.get_text(0, 60).ok()).filter(|s| !s.is_empty())
    } else { None };
    Some(Node { app: r.0.clone(), path: r.1.to_string(), role, name, showing: has(&state, SHOWING), sensitive: has(&state, SENSITIVE),
                editable, checked: has(&state, CHECKED), focused: has(&state, FOCUSED), text })
}

/// Depth-first over a subtree, bounded.
fn walk(conn: &Connection, from: &Ref) -> Vec<Node> {
    let mut out = vec![];
    let mut stack = vec![from.clone()];
    while let Some(r) = stack.pop() {
        if out.len() >= MAX_NODES { break; }
        if let Some(n) = read_node(conn, &r, true) { out.push(n); }
        if let Ok(a) = acc(conn, &r) {
            if let Ok(kids) = a.get_children() { stack.extend(kids.into_iter().rev()); }
        }
    }
    out
}

/// (app name, window title, window ref) for every top-level window on the bus.
fn windows(conn: &Connection) -> Result<Vec<(String, String, Ref)>, String> {
    let root = acc(conn, &(REGISTRY.to_string(), OwnedObjectPath::try_from(ROOT).unwrap())).map_err(|e| e.to_string())?;
    let mut v = vec![];
    for app in root.get_children().map_err(|e| e.to_string())? {
        let Ok(a) = acc(conn, &app) else { continue };
        let app_name = a.name().unwrap_or_default();
        let Ok(frames) = a.get_children() else { continue };
        for f in frames {
            if let Some(n) = read_node(conn, &f, false) {
                if n.showing { v.push((app_name.clone(), n.name, f)); }
            }
        }
    }
    Ok(v)
}

pub struct DesktopWorker(pub Rc<RefCell<DesktopState>>);

impl DesktopWorker {
    fn find_window(conn: &Connection, wanted: &str) -> Result<(String, Ref), String> {
        let w = wanted.to_lowercase();
        let all = windows(conn)?;
        let hits: Vec<_> = all.iter().filter(|(app, title, _)| app.to_lowercase().contains(&w) || title.to_lowercase().contains(&w)).collect();
        match hits.len() {
            0 => Err(format!("no window matches {wanted}; look with no window to see what is open")),
            1 => Ok((format!("{} — {}", hits[0].0, hits[0].1), hits[0].2.clone())),
            n => Err(format!("{n} windows match {wanted}: {}; say which", hits.iter().map(|(a, t, _)| format!("{a} — {t}")).collect::<Vec<_>>().join(", "))),
        }
    }

    /// The object behind an id, after the echoed name (if any) and liveness are checked.
    fn resolve(st: &DesktopState, conn: &Connection, id: u32, echoed: Option<&str>) -> Result<Ref, String> {
        let e = st.ids.get(id).ok_or_else(|| format!("control {id} was never handed out; look first"))?;
        if let Some(n) = echoed {
            if n != e.name { return Err(format!("control {id} is named {} now, not {n}; look again", e.name)); }
        }
        let r: Ref = (e.app.clone(), OwnedObjectPath::try_from(e.path.as_str()).map_err(|e| e.to_string())?);
        match acc(conn, &r).ok().and_then(|a| a.get_role().ok()) {
            Some(_) => Ok(r),
            None => Err("that control is gone; look again".into()),
        }
    }

    fn run_inner(&self, action: &Action) -> Result<String, String> {
        let mut st = self.0.borrow_mut();
        let cap = st.cap;
        let displays = st.displays.clone();
        match action {
            Action::OpenApp { name, visible } => {
                if !valid_app_name(name) { return Err(format!("invalid application name: {name}")); }
                let entry = format!("{name}.desktop");
                let dirs = ["/usr/share/applications", "/usr/local/share/applications", "/home/ai/.local/share/applications"];
                if !dirs.iter().any(|d| std::path::Path::new(d).join(&entry).is_file()) {
                    return Err(format!("no application called {name} is installed (no {entry} in the application folders)"));
                }
                let display = if *visible { &displays.visible } else { &displays.invisible };
                let conn = st.conn()?.clone();
                let before = windows(&conn)?.len();
                std::process::Command::new("gtk-launch").arg(name)
                    .env("WAYLAND_DISPLAY", display).env("GNOME_ACCESSIBILITY", "1").env_remove("DISPLAY")
                    .stdout(std::process::Stdio::null()).stderr(std::process::Stdio::null())
                    .spawn().map_err(|e| format!("could not launch {name}: {e}"))?;
                let t = Instant::now();
                while t.elapsed() < Duration::from_secs(10) {
                    std::thread::sleep(Duration::from_millis(500));
                    let now = windows(&conn)?;
                    if now.len() > before {
                        let (app, title, _) = now.last().unwrap();
                        return Ok(format!("opened {name}; its window is {app} — {title}"));
                    }
                }
                Ok(format!("opened {name}; no window appeared within 10 s, look again later"))
            }
            Action::Look { window: None, .. } => {
                let conn = st.conn()?.clone();
                let all = windows(&conn)?;
                if all.is_empty() { return Ok("no windows are open".into()); }
                Ok(format!("windows:\n{}", all.iter().map(|(a, t, _)| format!("- {a} — {t}")).collect::<Vec<_>>().join("\n")))
            }
            Action::Look { window: Some(w), find } => {
                let conn = st.conn()?.clone();
                let (title, frame) = Self::find_window(&conn, w)?;
                let nodes = walk(&conn, &frame);
                let chosen = select(&nodes, find.as_deref());
                Ok(render_look(&title, &chosen, &mut st.ids, cap))
            }
            Action::Press { control, name } => {
                let conn = st.conn()?.clone();
                let r = Self::resolve(&st, &conn, *control, Some(name))?;
                let a = ActionIfaceProxy::builder(&conn).destination(r.0.clone()).and_then(|b| b.path(r.1.clone())).and_then(|b| b.cache_properties(zbus::proxy::CacheProperties::No).build())
                    .map_err(|e| e.to_string())?;
                // A bus fault is never reported as a fact about the control: "no action" is only
                // ever an answer the control gave.
                match a.nactions() {
                    Ok(n) if n >= 1 => {}
                    Ok(_) => return Err(format!("{name} has no action to press")),
                    Err(e) => return Err(format!("could not ask {name} for its actions: {e}")),
                }
                a.do_action(0).map_err(|e| format!("press failed: {e}"))?;
                Ok(format!("pressed {name}"))
            }
            Action::Type { control, text, replace } => {
                let conn = st.conn()?.clone();
                let r = Self::resolve(&st, &conn, *control, None)?;
                let t = text_at(&conn, &r).map_err(|e| e.to_string())?;
                let e = EditableTextProxy::builder(&conn).destination(r.0.clone()).and_then(|b| b.path(r.1.clone())).and_then(|b| b.build()).map_err(|e| e.to_string())?;
                let mut count = t.character_count().map_err(|_| "that control takes no text".to_string())?;
                if *replace && count > 0 { e.delete_text(0, count).map_err(|e| format!("could not clear it: {e}"))?; count = 0; }
                let n = text.chars().count() as i32;
                e.insert_text(count, text, n).map_err(|e| format!("could not type: {e}"))?;
                let after = t.character_count().unwrap_or(count + n);
                Ok(format!("typed {n} characters into control {control}; it now holds {after} characters"))
            }
            Action::Read { control, from_line, lines } => {
                let conn = st.conn()?.clone();
                let r = Self::resolve(&st, &conn, *control, None)?;
                let t = text_at(&conn, &r).map_err(|e| e.to_string())?;
                let count = t.character_count().map_err(|_| "that control has no text".to_string())?;
                let all = t.get_text(0, count).map_err(|e| e.to_string())?;
                let v: Vec<&str> = all.lines().collect();
                let from = from_line.unwrap_or(1).max(1);
                let n = lines.unwrap_or(200).min(200);
                let slice: Vec<&str> = v.iter().skip(from - 1).take(n).copied().collect();
                Ok(format!("(lines {}-{} of {})\n{}", from, from + slice.len().saturating_sub(1), v.len(), slice.join("\n")))
            }
            other => Err(format!("not a desktop action: {other:?}")),
        }
    }
}

impl Worker for DesktopWorker {
    fn run(&self, action: &Action) -> Outcome {
        match self.run_inner(action) { Ok(d) => Outcome::ok(d), Err(e) => Outcome::err(e) }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The state bits are read out of one `au` the way AT-SPI packs them: bit 32 and up live
    /// in the second word, so a `showing` (25) in word 0 and a `visible` (30) never collide.
    #[test]
    fn state_bits_are_read_out_of_the_right_word() {
        let state = [(1 << CHECKED) | (1 << SHOWING), 0];
        assert!(has(&state, CHECKED) && has(&state, SHOWING));
        assert!(!has(&state, EDITABLE) && !has(&state, FOCUSED) && !has(&state, SENSITIVE));
        assert!(!has(&[], SHOWING), "a control that answered nothing is not showing");
        // 33 is ATSPI_STATE_REQUIRED, in the second word.
        assert!(has(&[0, 1 << 1], 33));
        assert!(!has(&[1 << 1, 0], 33), "the second word's bits are never read out of the first");
    }

    /// Every word `role_name` produces has to be one `desktop::select` keeps, or the hand would
    /// hand the model a control it then filters away.
    #[test]
    fn every_named_role_is_one_the_selection_keeps() {
        let named: Vec<&str> = (0..132).map(role_name).filter(|r| *r != "other").collect();
        assert_eq!(named.len(), 20, "{named:?}");
        for r in named {
            let n = Node { app: "a".into(), path: "/p".into(), role: r.into(), name: "x".into(), showing: true,
                           sensitive: true, editable: false, checked: false, focused: false, text: None };
            assert_eq!(select(std::slice::from_ref(&n), None).len(), 1, "select drops {r}");
        }
        assert_eq!(role_name(61), "text", "the editor's text view is what the live check types into");
        assert_eq!(role_name(99), "other", "grouping is furniture");
    }
}
