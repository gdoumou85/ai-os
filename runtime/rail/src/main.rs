//! The rail (1d design §4): a tall window of cards and a text box; a client of ai-os-engine.
use aios_proto::{Client, Event};
use aios_rail::cards::{Card, CardKind, Cards, Change};
use gtk4 as gtk;
use gtk::prelude::*;
use gtk::glib;
use std::cell::RefCell;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::mpsc::{channel, Sender};
use std::time::Duration;

fn socket_path() -> PathBuf {
    let dir = std::env::var("XDG_RUNTIME_DIR").unwrap_or_else(|_| "/run/user/1000".into());
    PathBuf::from(dir).join("ai-os.sock")
}

enum FromNet { Event(Event), Down, Up }

/// The socket on its own thread: reconnects every 3 s; every event goes to a plain std channel
/// that the GTK loop drains on a timer (gtk4-rs 0.11 has no glib channel; std::mpsc + a 50 ms
/// `timeout_add_local` needs no extra crate).
fn net_thread(to_ui: Sender<FromNet>) -> Sender<String> {
    let (say_tx, say_rx) = channel::<String>();
    std::thread::spawn(move || loop {
        match Client::connect(&socket_path()) {
            Ok(client) => {
                let (mut reader, mut writer) = client.split();
                let _ = to_ui.send(FromNet::Up);
                let _ = writer.hello();
                let ui = to_ui.clone();
                let pump = std::thread::spawn(move || { while let Some(e) = reader.next_event() { if ui.send(FromNet::Event(e)).is_err() { break } } });
                while !pump.is_finished() {
                    match say_rx.recv_timeout(Duration::from_millis(200)) {
                        Ok(t) => { if writer.say(&t).is_err() { break } }
                        Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
                        Err(_) => return,
                    }
                }
                let _ = to_ui.send(FromNet::Down);
                std::thread::sleep(Duration::from_secs(3));
            }
            Err(_) => { let _ = to_ui.send(FromNet::Down); std::thread::sleep(Duration::from_secs(3)); }
        }
    });
    say_tx
}

fn open_path(path: &str) {
    // Only paths the service sent reach here (cards.rs builds `opens` from events alone).
    let _ = std::process::Command::new("xdg-open").arg(path).stdout(std::process::Stdio::null()).stderr(std::process::Stdio::null()).spawn();
}

fn render(card: &Card, say: &Sender<String>) -> gtk::Widget {
    let b = gtk::Box::new(gtk::Orientation::Vertical, 4);
    b.add_css_class("card");
    let title = |t: &str| { let l = gtk::Label::new(Some(t)); l.add_css_class("title"); l.set_xalign(0.0); l.set_wrap(true); l };
    let text = |t: &str| { let l = gtk::Label::new(Some(t)); l.set_xalign(0.0); l.set_wrap(true); l.set_selectable(true); l };
    match &card.kind {
        CardKind::You => { b.add_css_class("you"); b.append(&text(&card.text)); }
        CardKind::Said => { b.add_css_class("said"); b.append(&text(&card.text)); }
        CardKind::Building { name, understood, steps, collapsed } => {
            b.add_css_class("building");
            b.append(&title(&format!("Building: {name}")));
            if !collapsed {
                b.append(&text(understood));
                for s in steps {
                    let mark = if !s.done { "☐" } else if s.ok { "✓" } else { "✗" };
                    b.append(&text(&format!("{mark} {}", s.text)));
                    if let Some(d) = &s.detail { let l = text(d); l.add_css_class("dim"); b.append(&l); }
                }
            }
        }
        CardKind::NeedsAnswer { questions } => { b.add_css_class("ask"); b.append(&title("Needs your answer")); for q in questions { b.append(&text(q)); } }
        CardKind::NeedsOk { what, why } => { b.add_css_class("ok"); b.append(&title("Needs your OK")); b.append(&text(what)); let l = text(why); l.add_css_class("dim"); b.append(&l); }
        CardKind::Done { text: t, check, files } => {
            b.add_css_class("done"); b.append(&title("Done")); b.append(&text(t));
            if let Some(c) = check { let l = text(&format!("check: {c}")); l.add_css_class("dim"); b.append(&l); }
            for f in files {
                if card.thumbnails.contains(&f.path) { let p = gtk::Picture::for_filename(&f.path); p.set_size_request(-1, 200); p.set_can_shrink(true); b.append(&p); }
                let row = gtk::Box::new(gtk::Orientation::Horizontal, 6);
                let name = f.path.rsplit('/').next().unwrap_or(&f.path).to_string();
                let l = text(&name); l.set_hexpand(true); row.append(&l);
                let open = gtk::Button::with_label("Open"); let path = f.path.clone(); open.connect_clicked(move |_| open_path(&path)); row.append(&open);
                b.append(&row);
            }
        }
        CardKind::Failed { text: t, files } | CardKind::Stopped { text: t, files } => {
            b.add_css_class("failed"); b.append(&title(if matches!(card.kind, CardKind::Failed { .. }) { "Could not finish" } else { "Stopped" })); b.append(&text(t));
            for f in files { let row = gtk::Box::new(gtk::Orientation::Horizontal, 6); let name = f.path.rsplit('/').next().unwrap_or(&f.path).to_string(); let l = text(&name); l.set_hexpand(true); row.append(&l); let open = gtk::Button::with_label("Open"); let path = f.path.clone(); open.connect_clicked(move |_| open_path(&path)); row.append(&open); b.append(&row); }
        }
        CardKind::Undone { lines, notes } => {
            b.add_css_class("undone"); b.append(&title("Undone"));
            for l in lines { b.append(&text(&format!("{} {}", if l.ok { "✓" } else { "✗" }, l.text))); }
            for n in notes { let l = text(n); l.add_css_class("dim"); b.append(&l); }
        }
    }
    if !card.buttons.is_empty() {
        let row = gtk::Box::new(gtk::Orientation::Horizontal, 6);
        for btn in &card.buttons { let w = gtk::Button::with_label(&btn.label); let s = say.clone(); let t = btn.say.clone(); w.connect_clicked(move |_| { let _ = s.send(t.clone()); }); row.append(&w); }
        b.append(&row);
    }
    b.upcast()
}

const CSS: &str = "
.card { padding: 8px 10px; margin: 4px 8px; border-radius: 8px; background: alpha(@theme_fg_color, 0.06); }
.you { background: alpha(@theme_selected_bg_color, 0.25); margin-left: 40px; }
.said { margin-right: 40px; }
.building { border-left: 3px solid @theme_selected_bg_color; }
.ok { border-left: 3px solid #e0a020; }
.ask { border-left: 3px solid #4090e0; }
.done { border-left: 3px solid #40b060; }
.failed { border-left: 3px solid #d04040; }
.title { font-weight: bold; }
.dim { opacity: 0.7; font-size: 90%; }
.status { opacity: 0.6; font-style: italic; margin: 2px 12px; }
";

fn main() {
    let app = gtk::Application::builder().application_id("org.aios.Rail").build();
    app.connect_activate(|app| {
        let css = gtk::CssProvider::new(); css.load_from_string(CSS);
        gtk::style_context_add_provider_for_display(&gtk::gdk::Display::default().unwrap(), &css, gtk::STYLE_PROVIDER_PRIORITY_APPLICATION);
        let win = gtk::ApplicationWindow::builder().application(app).title("AI OS").default_width(420).default_height(900).build();
        let column = gtk::Box::new(gtk::Orientation::Vertical, 0);
        let scroll = gtk::ScrolledWindow::builder().vexpand(true).child(&column).build();
        let status = gtk::Label::new(Some("Connecting to the AI OS service…")); status.add_css_class("status"); status.set_xalign(0.0);
        let entry = gtk::Entry::builder().placeholder_text("Tell the AI what you want…").margin_start(8).margin_end(8).margin_bottom(8).build();
        let root = gtk::Box::new(gtk::Orientation::Vertical, 4);
        root.append(&scroll); root.append(&status); root.append(&entry);
        win.set_child(Some(&root));

        let (to_ui, from_net) = channel::<FromNet>();
        let say = net_thread(to_ui);
        let cards = Rc::new(RefCell::new(Cards::default()));
        let widgets: Rc<RefCell<Vec<gtk::Widget>>> = Rc::default();

        let s = say.clone();
        entry.connect_activate(move |e| { let t = e.text().trim().to_string(); if !t.is_empty() { let _ = s.send(t); e.set_text(""); } });

        let (cards2, widgets2, column2, status2, scroll2, say2) = (cards.clone(), widgets.clone(), column.clone(), status.clone(), scroll.clone(), say.clone());
        glib::timeout_add_local(Duration::from_millis(50), move || {
            while let Ok(msg) = from_net.try_recv() {
                match msg {
                    FromNet::Up => status2.set_text(""),
                    FromNet::Down => status2.set_text("The AI OS service is not running — retrying…"),
                    FromNet::Event(ev) => {
                        // `apply` on its own line: the RefMut must end before `render` borrows.
                        let changes = cards2.borrow_mut().apply(&ev);
                        for ch in changes {
                            match ch {
                                Change::Added(i) => { let w = render(&cards2.borrow().list[i], &say2); column2.append(&w); widgets2.borrow_mut().push(w); }
                                Change::Updated(i) => { let old = widgets2.borrow()[i].clone(); let w = render(&cards2.borrow().list[i], &say2); column2.insert_child_after(&w, Some(&old)); column2.remove(&old); widgets2.borrow_mut()[i] = w; }
                                Change::Line(t) => status2.set_text(&t),
                            }
                        }
                        let adj = scroll2.vadjustment(); adj.set_value(adj.upper());
                    }
                }
            }
            glib::ControlFlow::Continue
        });
        entry.grab_focus();
        win.present();
    });
    app.run();
}
