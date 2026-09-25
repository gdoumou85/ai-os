//! The rail (1d design §4): a tall window of cards and a text box; a client of ai-os-engine.
mod pages;

use aios_proto::{ChangedFile, Client, Event, Request};
use aios_rail::cards::{Card, CardKind, Cards, Change};
use gtk4 as gtk;
use gtk::prelude::*;
use gtk::glib;
use pages::model::{cloud_accounts, cloud_card, cloud_models_card, cloud_on, config_dir, engine_env, models_card, run, write_private};
use std::cell::RefCell;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::mpsc::{channel, Sender};
use std::time::Duration;

/// The line under the cards, with the clock it is timed by: a local model can think for
/// minutes, and a line that never changes looks stuck (the owner, 2026-09-20).
type Live = Rc<RefCell<(String, std::time::Instant)>>;

fn set_status(status: &gtk::Label, live: &Live, text: &str) {
    *live.borrow_mut() = (text.to_string(), std::time::Instant::now());
    status.set_text(text);
}

fn waited(secs: u64) -> String {
    if secs < 90 { format!("{secs} seconds") } else { format!("{} min", secs / 60) }
}

fn socket_path() -> PathBuf {
    let dir = std::env::var("XDG_RUNTIME_DIR").unwrap_or_else(|_| {
        use std::os::unix::fs::MetadataExt;
        let uid = std::fs::metadata("/proc/self").map(|m| m.uid()).unwrap_or(0);
        format!("/run/user/{uid}")
    });
    PathBuf::from(dir).join("ai-os.sock")
}

pub(crate) enum FromNet {
    Event(Event), Down, Up, Update(String),
    /// The finder's lines and the engine's current environment, for the Model card.
    Models { found: String, env: String },
    /// One LM Studio's models, asked for with the key the person typed.
    Keyed { url: String, key: String, found: String },
    /// The cloud models an account's key opened, for the card that adds one.
    Cloud { provider: aios_rail::models::Provider, key: String, found: String, replace: Option<usize> },
}

/// The socket on its own thread: reconnects every 3 s; every event goes to a plain std channel
/// that the GTK loop drains on a timer (gtk4-rs 0.11 has no glib channel; std::mpsc + a 50 ms
/// `timeout_add_local` needs no extra crate).
fn net_thread(to_ui: Sender<FromNet>) -> Sender<Request> {
    let (say_tx, say_rx) = channel::<Request>();
    std::thread::spawn(move || loop {
        match Client::connect(&socket_path()) {
            Ok(client) => {
                let (mut reader, mut writer) = client.split();
                let _ = to_ui.send(FromNet::Up);
                let _ = writer.hello();
                let ui = to_ui.clone();
                let pump = std::thread::spawn(move || { while let Some(e) = reader.next_event() { if ui.send(FromNet::Event(e)).is_err() { break } } });
                let mut ui_gone = false;
                while !pump.is_finished() {
                    match say_rx.recv_timeout(Duration::from_millis(200)) {
                        Ok(r) => { if writer.request(&r).is_err() { break } }
                        Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
                        Err(_) => { ui_gone = true; break }
                    }
                }
                // The two halves are separate fds over one socket: dropping the writer would leave
                // the pump parked on a live connection the service never prunes, and the old and
                // new connections would both push events into the UI channel (every card twice).
                // Shut the socket down, then wait for the pump to end before reconnecting.
                writer.shutdown();
                let _ = pump.join();
                if ui_gone { return }
                let _ = to_ui.send(FromNet::Down);
                std::thread::sleep(Duration::from_secs(3));
            }
            Err(_) => { let _ = to_ui.send(FromNet::Down); std::thread::sleep(Duration::from_secs(3)); }
        }
    });
    say_tx
}

/// The terminal that runs the update: Ubuntu 26.04's Ptyxis, else GNOME Terminal, else whatever
/// the system calls its terminal.
fn run_in_terminal(cmd: &str) {
    let tries: [(&str, &[&str]); 3] = [("ptyxis", &["--", "bash", "-c"]), ("gnome-terminal", &["--", "bash", "-c"]), ("x-terminal-emulator", &["-e", "bash", "-c"])];
    for (prog, args) in tries {
        if std::process::Command::new(prog).args(args).arg(cmd).spawn().is_ok() { return; }
    }
}

/// The card that offers a newer version (update.rs). Not one of `Cards`: it is about the AI OS
/// itself, not the conversation, so Clear leaves it and it goes when answered.
fn update_card(version: &str, column: &gtk::Box) -> gtk::Widget {
    let b = gtk::Box::new(gtk::Orientation::Vertical, 4);
    b.add_css_class("card"); b.add_css_class("ask");
    let title = gtk::Label::new(Some("Update available")); title.add_css_class("title"); title.set_xalign(0.0);
    let text = gtk::Label::new(Some(&format!("A new version of the AI OS ({version}) is available. Updating keeps your model and settings, and asks for your password in a terminal."))); text.set_xalign(0.0); text.set_wrap(true);
    let row = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    let now = gtk::Button::with_label("Update now");
    let later = gtk::Button::with_label("Later");
    row.append(&now); row.append(&later);
    b.append(&title); b.append(&text); b.append(&row);
    let w: gtk::Widget = b.upcast();
    let (c1, w1) = (column.clone(), w.clone());
    now.connect_clicked(move |_| { run_in_terminal(&aios_rail::update::update_command()); c1.remove(&w1); });
    let (c2, w2) = (column.clone(), w.clone());
    later.connect_clicked(move |_| c2.remove(&w2));
    w
}

fn open_path(path: &str) {
    // Only paths the service sent reach here (cards.rs builds `opens` from events alone).
    let _ = std::process::Command::new("xdg-open").arg(path).stdout(std::process::Stdio::null()).stderr(std::process::Stdio::null()).spawn();
}

fn render(card: &Card, say: &Sender<Request>, entry: &gtk::Entry) -> gtk::Widget {
    let b = gtk::Box::new(gtk::Orientation::Vertical, 4);
    b.add_css_class("card");
    let title = |t: &str| { let l = gtk::Label::new(Some(t)); l.add_css_class("title"); l.set_xalign(0.0); l.set_wrap(true); l };
    let text = |t: &str| { let l = gtk::Label::new(Some(t)); l.set_xalign(0.0); l.set_wrap(true); l.set_selectable(true); l };
    // One block for every card that carries files: an image is a thumbnail whether the job
    // finished, failed or was stopped, and each file gets its own Open row.
    let file_rows = |into: &gtk::Box, files: &[ChangedFile]| {
        for f in files {
            if card.thumbnails.contains(&f.path) { let p = gtk::Picture::for_filename(&f.path); p.set_size_request(-1, 200); p.set_can_shrink(true); into.append(&p); }
            let row = gtk::Box::new(gtk::Orientation::Horizontal, 6);
            let name = f.path.rsplit('/').next().unwrap_or(&f.path).to_string();
            let l = text(&name); l.set_hexpand(true); row.append(&l);
            let open = gtk::Button::with_label("Open"); let path = f.path.clone(); open.connect_clicked(move |_| open_path(&path)); row.append(&open);
            into.append(&row);
        }
    };
    match &card.kind {
        CardKind::You => { b.add_css_class("you"); b.append(&text(&card.text)); }
        CardKind::Said => { b.add_css_class("said"); b.append(&text(&card.text)); }
        CardKind::Building { name, understood, steps, actions, collapsed, alert } => {
            b.add_css_class(if *alert { "alert" } else { "building" });
            b.append(&title(&if *alert { format!("🔔 Alert: {name}") } else if name.is_empty() { "Working".to_string() } else { format!("Building: {name}") }));
            if !collapsed {
                if !understood.is_empty() { b.append(&text(understood)); }
                for s in steps {
                    let mark = if !s.done { "☐" } else if s.ok { "✓" } else { "✗" };
                    b.append(&text(&format!("{mark} {}", s.text)));
                    if let Some(d) = &s.detail { let l = text(d); l.add_css_class("dim"); b.append(&l); }
                }
                // The newest twelve actions; a long turn would push the chat off the screen.
                if actions.len() > 12 { let l = text(&format!("… {} earlier", actions.len() - 12)); l.add_css_class("dim"); b.append(&l); }
                for a in actions.iter().skip(actions.len().saturating_sub(12)) {
                    let l = text(&format!("{} {}", if a.ok { "✓" } else { "✗" }, a.text)); l.add_css_class("dim"); b.append(&l);
                }
            }
        }
        CardKind::NeedsAnswer { questions, options } => {
            b.add_css_class("ask"); b.append(&title("Needs your answer"));
            // A click answers (cards::answer): one question sends at once; with several, the picks
            // gather in the text box and go when every question has one. Typing always works too.
            let picks = Rc::new(RefCell::new(vec![None::<String>; questions.len()]));
            for (i, q) in questions.iter().enumerate() {
                b.append(&text(q));
                let Some(opts) = options.get(i).filter(|o| !o.is_empty()) else { continue };
                let row = gtk::FlowBox::builder().selection_mode(gtk::SelectionMode::None).max_children_per_line(4).column_spacing(6).row_spacing(6).build();
                for o in opts {
                    let w = gtk::Button::with_label(o);
                    let (picks, qs, s, e, o) = (picks.clone(), questions.clone(), say.clone(), entry.clone(), o.clone());
                    w.connect_clicked(move |w| {
                        // Found from the button, not held in a list: a list the buttons' own handlers
                        // held would keep every answered card alive after Clear.
                        let mut c = w.ancestor(gtk::FlowBox::static_type()).and_then(|f| f.first_child());
                        while let Some(child) = c { if let Some(x) = child.first_child() { x.remove_css_class("suggested-action"); } c = child.next_sibling(); }
                        w.add_css_class("suggested-action");
                        picks.borrow_mut()[i] = Some(o.clone());
                        let (t, complete) = aios_rail::cards::answer(&qs, &picks.borrow());
                        if complete {
                            let _ = s.send(Request::Say(t)); e.set_text("");
                            if let Some(card) = w.ancestor(gtk::Box::static_type()) { card.set_sensitive(false); }
                        } else { e.set_text(&t); e.set_position(-1); e.grab_focus(); }
                    });
                    row.insert(&w, -1);
                }
                b.append(&row);
            }
        }
        CardKind::Done { text: t, check, files, learned } => {
            b.add_css_class("done"); b.append(&title("Done")); b.append(&text(t));
            if let Some(c) = check { let l = text(&format!("check: {c}")); l.add_css_class("dim"); b.append(&l); }
            for l in learned { let w = text(l); w.add_css_class("dim"); b.append(&w); }
            file_rows(&b, files);
        }
        CardKind::Failed { text: t, files } | CardKind::Stopped { text: t, files } => {
            b.add_css_class("failed"); b.append(&title(if matches!(card.kind, CardKind::Failed { .. }) { "Could not finish" } else { "Stopped" })); b.append(&text(t));
            file_rows(&b, files);
        }
    }
    if !card.buttons.is_empty() {
        let row = gtk::Box::new(gtk::Orientation::Horizontal, 6);
        for btn in &card.buttons { let w = gtk::Button::with_label(&btn.label); let s = say.clone(); let t = btn.say.clone(); w.connect_clicked(move |_| { let _ = s.send(if t == aios_rail::cards::CLEAR { Request::Clear {} } else { Request::Say(t.clone()) }); }); row.append(&w); }
        b.append(&row);
    }
    b.upcast()
}

const CSS: &str = "
.card { padding: 8px 10px; margin: 4px 8px; border-radius: 8px; background: alpha(@theme_fg_color, 0.06); }
.you { background: alpha(@theme_selected_bg_color, 0.25); margin-left: 40px; }
.said { margin-right: 40px; }
.building { border-left: 3px solid @theme_selected_bg_color; }
.alert { border-left: 3px solid #e08020; }
.ask { border-left: 3px solid #4090e0; }
.done { border-left: 3px solid #40b060; }
.failed { border-left: 3px solid #d04040; }
.title { font-weight: bold; }
.dim { opacity: 0.7; font-size: 90%; }
.status { opacity: 0.6; font-style: italic; margin: 2px 12px; }
.sidebar { background: alpha(@theme_fg_color, 0.04); padding: 6px; }
.nav { padding: 6px 10px; border-radius: 6px; }
.nav:checked { background: alpha(@theme_selected_bg_color, 0.25); font-weight: bold; }
.dot-on { color: #40b060; } .dot-urgent { color: #d04040; } .dot-off { color: alpha(@theme_fg_color, 0.4); }
";

fn main() {
    // The software renderer, unless the person chose one: GTK's GPU renderers left new cards
    // undrawn until a scroll in the owner's VirtualBox VM, and a column of text cards gains
    // nothing from the GPU on real hardware either.
    if std::env::var_os("GSK_RENDERER").is_none() { std::env::set_var("GSK_RENDERER", "cairo"); }
    let app = gtk::Application::builder().application_id("org.aios.Rail").build();
    app.connect_activate(|app| {
        let css = gtk::CssProvider::new(); css.load_from_string(CSS);
        gtk::style_context_add_provider_for_display(&gtk::gdk::Display::default().unwrap(), &css, gtk::STYLE_PROVIDER_PRIORITY_APPLICATION);
        let win = gtk::ApplicationWindow::builder().application(app).title("AI OS").default_width(570).default_height(600).build();
        // 600, not taller: a small screen (a VM's, a laptop's) put a taller window's bottom — the
        // newest card, its Yes button, the entry — below the edge. The cards scroll inside it.
        let header = gtk::HeaderBar::new();
        let clear = gtk::Button::with_label("Clear");
        clear.set_tooltip_text(Some("A clean start: the AI forgets the chat and what it was told to keep and stops a task still open (projects and skills stay)"));
        header.pack_start(&clear);
        // Stop where it can always be reached while a job runs, not on a card scrolled out of view.
        let stop = gtk::Button::with_label("Stop");
        stop.add_css_class("destructive-action");
        stop.set_visible(false);
        header.pack_end(&stop);
        win.set_titlebar(Some(&header));
        let column = gtk::Box::new(gtk::Orientation::Vertical, 0);
        let scroll = gtk::ScrolledWindow::builder().vexpand(true).child(&column).build();
        let status = gtk::Label::new(Some("Connecting to the AI OS service…")); status.add_css_class("status"); status.set_xalign(0.0);
        let entry = gtk::Entry::builder().placeholder_text("Tell the AI what you want…").margin_start(8).margin_end(8).margin_bottom(8).build();
        // The spinner beside the status line: turning while the AI works (cards::busy_after).
        let spinner = gtk::Spinner::new(); spinner.set_margin_start(12);
        let status_row = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        status_row.append(&spinner); status_row.append(&status);
        // The same line, retimed every five seconds while the AI works, so a long wait says
        // how long it has been waiting. Only while the spinner turns: anything else on this line
        // (a card's message, "service not running") is not a wait.
        let live: Live = Rc::new(RefCell::new((String::new(), std::time::Instant::now())));
        let (status_tick, live_tick, spinner_tick) = (status.clone(), live.clone(), spinner.clone());
        glib::timeout_add_seconds_local(5, move || {
            let (text, since) = &*live_tick.borrow();
            let secs = since.elapsed().as_secs();
            if spinner_tick.is_spinning() && !text.is_empty() && secs >= 20 {
                status_tick.set_text(&format!("{text} — {}", waited(secs)));
            }
            glib::ControlFlow::Continue
        });
        // The pages, and the menu on the left that picks one (sidebar design §3).
        let stack = gtk::Stack::new();
        stack.set_vexpand(true); stack.set_hexpand(true);
        stack.add_named(&scroll, Some("chat"));
        let projects_col = pages::page_column(); stack.add_named(&pages::scrolled(&projects_col), Some("projects"));
        let watchers_col = pages::page_column(); stack.add_named(&pages::scrolled(&watchers_col), Some("watchers"));
        let skills_col = pages::page_column(); stack.add_named(&pages::scrolled(&skills_col), Some("skills"));
        let model_col = pages::page_column(); stack.add_named(&pages::scrolled(&model_col), Some("model"));
        stack.add_named(&pages::help::page(), Some("help"));
        let chat_nav = pages::nav("💬 Chat", "chat", &stack, None);
        let projects_nav = pages::nav("📁 Projects", "projects", &stack, Some(&chat_nav));
        let watchers_nav = pages::nav(&aios_rail::sidebar::watchers_label(0), "watchers", &stack, Some(&chat_nav));
        let skills_nav = pages::nav("🧠 Skills", "skills", &stack, Some(&chat_nav));
        let model_nav = pages::nav("⚙ Model", "model", &stack, Some(&chat_nav));
        let help_nav = pages::nav("? Help", "help", &stack, Some(&chat_nav));
        chat_nav.set_active(true);
        // Stays on until turned off (the owner's call, 2026-09-19); the engine reads it every turn.
        let cloud = gtk::ToggleButton::with_label("☁ Cloud");
        cloud.add_css_class("nav");
        cloud.set_tooltip_text(Some("Use your cloud accounts: stronger models, but what you ask leaves this computer"));
        cloud.set_active(cloud_on());
        let menu = gtk::Box::new(gtk::Orientation::Vertical, 2);
        menu.add_css_class("sidebar"); menu.set_size_request(150, -1);
        let gap = gtk::Box::new(gtk::Orientation::Vertical, 0); gap.set_vexpand(true);
        for w in [&chat_nav, &projects_nav, &watchers_nav, &skills_nav] { menu.append(w); }
        menu.append(&gap);
        menu.append(&model_nav); menu.append(&cloud); menu.append(&help_nav);
        // The message box belongs to the chat.
        let e_vis = entry.clone();
        stack.connect_visible_child_name_notify(move |s| e_vis.set_visible(s.visible_child_name().as_deref() == Some("chat")));
        let main_area = gtk::Box::new(gtk::Orientation::Vertical, 4);
        main_area.append(&stack); main_area.append(&status_row); main_area.append(&entry);
        let root = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        root.append(&menu); root.append(&gtk::Separator::new(gtk::Orientation::Vertical)); root.append(&main_area);
        win.set_child(Some(&root));

        // Stay at the bottom, but only once GTK has allocated the new card: `upper` grows when the
        // child is laid out, which is after the event drain returns, so scrolling there left the
        // newest card below the fold until something else moved the view.
        // Moved on the next idle rather than inside `changed`: setting the value while GTK is still
        // laying the card out left the view drawn stale until the person scrolled.
        scroll.vadjustment().connect_changed(|a| {
            let a = a.clone();
            glib::idle_add_local_once(move || a.set_value(a.upper() - a.page_size()));
        });

        let (to_ui, from_net) = channel::<FromNet>();
        // Once per login, off the GTK thread: a slow network never holds the window up.
        let up = to_ui.clone();
        std::thread::spawn(move || { if let Some(v) = aios_rail::update::check() { let _ = up.send(FromNet::Update(v)); } });
        let (to_ui_models, to_ui2, to_ui_cloud) = (to_ui.clone(), to_ui.clone(), to_ui.clone());
        let say = net_thread(to_ui);
        let cards = Rc::new(RefCell::new(Cards::default()));
        let widgets: Rc<RefCell<Vec<gtk::Widget>>> = Rc::default();

        let s_skills = say.clone();
        skills_nav.connect_toggled(move |b| if b.is_active() { let _ = s_skills.send(Request::Skills {}); });
        let s_projects = say.clone();
        projects_nav.connect_toggled(move |b| if b.is_active() { let _ = s_projects.send(Request::Projects {}); });
        // Alerts since the owner last looked at the Watchers page (watchers design §2).
        let unseen = Rc::new(std::cell::Cell::new(0usize));
        let (s_watchers, unseen_w) = (say.clone(), unseen.clone());
        watchers_nav.connect_toggled(move |b| if b.is_active() {
            unseen_w.set(0);
            b.set_label(&aios_rail::sidebar::watchers_label(0));
            let _ = s_watchers.send(Request::Watchers {});
        });
        let (tx_m, cards_m, status_m, col_m, cloud_m) = (to_ui_models, cards.clone(), status.clone(), model_col.clone(), cloud.clone());
        model_nav.connect_toggled(move |b| {
            if !b.is_active() { return }
            pages::empty(&col_m);
            if cards_m.borrow().running() {
                status_m.set_text("Finish or stop the task first, then switch models.");
                col_m.append(&pages::msg("Finish or stop the task first, then switch models."));
                // Adding an account restarts nothing, so it is fine while a task runs.
                if cloud_m.is_active() && aios_rail::models::accounts(&cloud_accounts()).is_empty() { col_m.append(&cloud_card(&col_m, &tx_m, &status_m)); }
                return;
            }
            status_m.set_text("Looking for models on your network…");
            let tx = tx_m.clone();
            std::thread::spawn(move || { let found = run("ai-os-find", &[], None); let _ = tx.send(FromNet::Models { found, env: engine_env() }); });
        });
        let (model_nav2, model_col_c, tx_c) = (model_nav.clone(), model_col.clone(), to_ui_cloud);
        let st_c = status.clone();
        cloud.connect_toggled(move |t| {
            let flag = format!("{}/cloud-on", config_dir());
            let done = if t.is_active() { write_private(&flag, "") } else { std::fs::remove_file(&flag).or_else(|e| if e.kind() == std::io::ErrorKind::NotFound { Ok(()) } else { Err(e) }).map_err(|e| e.to_string()) };
            if let Err(e) = done { st_c.set_text(&format!("Could not switch the cloud: {e}")); return }
            // Nothing to use yet: the card to add one, on the Model page — shown at once if it is
            // already the page up, since a toggle already active emits no `toggled` to react to.
            if t.is_active() && aios_rail::models::accounts(&cloud_accounts()).is_empty() {
                if model_nav2.is_active() { model_col_c.append(&cloud_card(&model_col_c, &tx_c, &st_c)); }
                else { model_nav2.set_active(true); }
            }
            st_c.set_text(if t.is_active() { "Cloud is on: your cloud accounts answer first." } else { "Cloud is off: only your own models answer." });
        });
        let s_stop = say.clone();
        stop.connect_clicked(move |_| { let _ = s_stop.send(Request::Say("stop".into())); });
        // The screen empties when the service says `cleared`, so every open rail does.
        let s_clear = say.clone();
        clear.connect_clicked(move |_| { let _ = s_clear.send(Request::Clear {}); });

        let s = say.clone();
        entry.connect_activate(move |e| { let t = e.text().trim().to_string(); if !t.is_empty() { let _ = s.send(Request::Say(t)); e.set_text(""); } });

        let (cards2, widgets2, column2, status2, say2, spinner2, stop2, entry2, cloud2) = (cards.clone(), widgets.clone(), column.clone(), status.clone(), say.clone(), spinner.clone(), stop.clone(), entry.clone(), cloud.clone());
        let (model_col2, projects_col2, skills_col2, chat_nav2) = (model_col.clone(), projects_col.clone(), skills_col.clone(), chat_nav.clone());
        let (watchers_col2, watchers_nav2, unseen2) = (watchers_col.clone(), watchers_nav.clone(), unseen.clone());
        let live2 = live.clone();
        glib::timeout_add_local(Duration::from_millis(50), move || {
            while let Ok(msg) = from_net.try_recv() {
                match msg {
                    FromNet::Up => status2.set_text(""),
                    FromNet::Update(v) => column2.prepend(&update_card(&v, &column2)),
                    FromNet::Models { found, env } => {
                        status2.set_text("");
                        pages::empty(&model_col2);
                        model_col2.append(&models_card(&found, &env, None, &model_col2, &to_ui2, &status2));
                        // Cloud on with no account yet: the card to add one, under the models.
                        if cloud2.is_active() && aios_rail::models::accounts(&cloud_accounts()).is_empty() { model_col2.append(&cloud_card(&model_col2, &to_ui2, &status2)); }
                    }
                    FromNet::Keyed { url, key, found } => {
                        // Only that runner's lines, and none still asking for a key: the key worked.
                        let only: String = found.lines().filter(|l| l.split('\t').nth(1) == Some(url.as_str())).map(|l| format!("{l}\n")).collect();
                        if only.is_empty() || only.contains("\t-\t") { status2.set_text("LM Studio did not accept that key."); }
                        else { model_col2.append(&models_card(&only, &engine_env(), Some(key), &model_col2, &to_ui2, &status2)); }
                    }
                    FromNet::Cloud { provider, key, found, replace } => { status2.set_text(""); model_col2.append(&cloud_models_card(provider, &found, key, replace, &model_col2, &status2, &cloud2)); }
                    FromNet::Down => { spinner2.stop(); status2.set_text("The AI OS service is not running — retrying…"); }
                    FromNet::Event(ev) => {
                        if let Event::Skills { notebooks } = &ev { pages::skills::fill(&skills_col2, notebooks, &say2); continue; }
                        if let Event::Projects { projects } = &ev { pages::projects::fill(&projects_col2, projects, &entry2, &chat_nav2); continue; }
                        if let Event::Watchers { watchers } = &ev { pages::watchers::fill(&watchers_col2, watchers, &say2); continue; }
                        // Counted before the card brings the chat forward.
                        if matches!(ev, Event::Alert { .. }) && !watchers_nav2.is_active() {
                            unseen2.set(unseen2.get() + 1);
                            watchers_nav2.set_label(&aios_rail::sidebar::watchers_label(unseen2.get()));
                        }
                        match aios_rail::cards::busy_after(&ev) {
                            Some(true) => { spinner2.start(); set_status(&status2, &live2, "The AI is thinking…"); }
                            Some(false) => { spinner2.stop(); set_status(&status2, &live2, ""); }
                            None => {}
                        }
                        // `apply` on its own line: the RefMut must end before `render` borrows.
                        let changes = cards2.borrow_mut().apply(&ev);
                        if let Event::Cleared {} = ev {
                            for w in widgets2.borrow_mut().drain(..) { column2.remove(&w); }
                            for c in &cards2.borrow().list { let w = render(c, &say2, &entry2); column2.append(&w); widgets2.borrow_mut().push(w); }
                        }
                        for ch in changes {
                            match ch {
                                Change::Added(i) => { chat_nav2.set_active(true); let w = render(&cards2.borrow().list[i], &say2, &entry2); column2.append(&w); widgets2.borrow_mut().push(w); }
                                Change::Updated(i) => { let old = widgets2.borrow()[i].clone(); let w = render(&cards2.borrow().list[i], &say2, &entry2); column2.insert_child_after(&w, Some(&old)); column2.remove(&old); widgets2.borrow_mut()[i] = w; }
                                // What the AI is doing this second (engine `tick`): the step it is
                                // on and the command or file it is working, while it works it.
                                Change::Line(t) => set_status(&status2, &live2, &t),
                            }
                        }
                        stop2.set_visible(cards2.borrow().running());
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
