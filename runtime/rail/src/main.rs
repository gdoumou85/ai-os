//! The rail (1d design §4): a tall window of cards and a text box; a client of ai-os-engine.
use aios_proto::{ChangedFile, Client, Event};
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
    let dir = std::env::var("XDG_RUNTIME_DIR").unwrap_or_else(|_| {
        use std::os::unix::fs::MetadataExt;
        let uid = std::fs::metadata("/proc/self").map(|m| m.uid()).unwrap_or(0);
        format!("/run/user/{uid}")
    });
    PathBuf::from(dir).join("ai-os.sock")
}

enum FromNet {
    Event(Event), Down, Up, Update(String),
    /// The finder's lines and the engine's current environment, for the Model card.
    Models { found: String, env: String },
    /// One LM Studio's models, asked for with the key the person typed.
    Keyed { url: String, key: String, found: String },
    /// The cloud models an account's key opened, for the card that adds one.
    Cloud { provider: aios_rail::models::Provider, key: String, found: String },
}

/// Where the chat window and the engine's cloud pool share the switch and the accounts (core::cloud).
fn config_dir() -> String { format!("{}/.config/ai-os", std::env::var("HOME").unwrap_or_default()) }
fn cloud_on() -> bool { std::path::Path::new(&format!("{}/cloud-on", config_dir())).exists() }
fn cloud_accounts() -> String { std::fs::read_to_string(format!("{}/cloud.tsv", config_dir())).unwrap_or_default() }

/// A file only its owner can read: the keys are in it.
fn write_private(path: &str, text: &str) -> Result<(), String> {
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;
    std::fs::create_dir_all(config_dir()).map_err(|e| e.to_string())?;
    let mut f = std::fs::OpenOptions::new().write(true).create(true).truncate(true).mode(0o600).open(path).map_err(|e| e.to_string())?;
    f.write_all(text.as_bytes()).map_err(|e| e.to_string())
}

/// The cloud accounts card: each account with Remove, and a key box with a button per provider.
/// ponytail: NVIDIA, OpenRouter and Ollama; the other OpenAI-style providers (Groq, Mistral…) get a button
/// each once someone has a key to test them with.
fn cloud_card(column: &gtk::Box, to_ui: &Sender<FromNet>, status: &gtk::Label) -> gtk::Widget {
    let tsv = cloud_accounts();
    let list = aios_rail::models::accounts(&tsv);
    let text = if list.is_empty() { "No cloud accounts yet. While Cloud is on, they are tried in order, and the next takes over when one's free allowance runs out.".to_string() }
        else { "Tried in this order while Cloud is on; the next takes over when one's free allowance runs out.".to_string() };
    let b = plain_card("Cloud accounts", &text);
    let w: gtk::Widget = b.clone().upcast();
    for (i, (name, model)) in list.into_iter().enumerate() {
        let row = gtk::Box::new(gtk::Orientation::Horizontal, 6);
        let l = gtk::Label::new(Some(&format!("{}. {name}: {model}", i + 1))); l.set_xalign(0.0); l.set_hexpand(true); l.set_wrap(true);
        let rm = gtk::Button::with_label("Remove");
        let (col, me, st, tx, stt) = (column.clone(), w.clone(), status.clone(), to_ui.clone(), status.clone());
        rm.connect_clicked(move |_| {
            let _ = write_private(&format!("{}/cloud.tsv", config_dir()), &aios_rail::models::without(&cloud_accounts(), i));
            st.set_text(&format!("Removed {name}."));
            col.remove(&me);
            col.append(&cloud_card(&col, &tx, &stt));
        });
        row.append(&l); row.append(&rm);
        b.append(&row);
    }
    let add = gtk::Label::new(Some("Add an account: paste its key here and press its provider.
• NVIDIA: make a key at build.nvidia.com (sign in, then Get API Key).
• OpenRouter: openrouter.ai → Keys (its free models end in \":free\").
• Ollama: ollama.com → Settings → Keys."));
    add.set_xalign(0.0); add.set_wrap(true);
    let entry = gtk::PasswordEntry::new(); entry.set_show_peek_icon(true);
    let row = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    let close = gtk::Button::with_label("Close"); close.set_halign(gtk::Align::Start);
    b.append(&add); b.append(&entry); b.append(&row); b.append(&close);
    for provider in [aios_rail::models::NVIDIA, aios_rail::models::OPENROUTER, aios_rail::models::OLLAMA] {
        let go = gtk::Button::with_label(&format!("{} key", provider.name));
        row.append(&go);
        let (col, me, tx, st, entry) = (column.clone(), w.clone(), to_ui.clone(), status.clone(), entry.clone());
        go.connect_clicked(move |_| {
            let key = entry.text().to_string();
            if !aios_rail::models::safe_key(&key) { st.set_text("That key has spaces or odd characters in it; paste just the key."); return; }
            col.remove(&me);
            st.set_text(&format!("Asking {} which models your key opens…", provider.name));
            let tx = tx.clone();
            std::thread::spawn(move || { let found = run("ai-os-find", &["--url", provider.url], Some(&key)); let _ = tx.send(FromNet::Cloud { provider, key, found }); });
        });
    }
    let (col, me) = (column.clone(), w.clone());
    close.connect_clicked(move |_| col.remove(&me));
    w
}

/// The models a provider's key opens, a button each: a click adds the account and turns Cloud on.
fn cloud_models_card(provider: aios_rail::models::Provider, found: &str, key: String, column: &gtk::Box, status: &gtk::Label, switch: &gtk::ToggleButton) -> gtk::Widget {
    let choices = aios_rail::models::chat_models(provider, aios_rail::models::parse_found(found).into_iter().filter_map(|c| c.model).collect());
    let empty = format!("{} did not list any models for that key. Check the key and try again.", provider.name);
    let b = plain_card("Pick a cloud model", if choices.is_empty() { &empty } else { "The bigger the model, the better it works, and the sooner its free allowance runs out." });
    let w: gtk::Widget = b.clone().upcast();
    for m in choices {
        let btn = gtk::Button::with_label(&m); btn.set_halign(gtk::Align::Start);
        let (col, me, st, sw, key) = (column.clone(), w.clone(), status.clone(), switch.clone(), key.clone());
        btn.connect_clicked(move |_| {
            col.remove(&me);
            let Some(line) = aios_rail::models::account_line(provider.name, provider.kind, provider.url, &m, &key) else { st.set_text("That model's name has characters it may not."); return };
            match write_private(&format!("{}/cloud.tsv", config_dir()), &(cloud_accounts() + &line)) {
                Ok(()) => { sw.set_active(true); st.set_text(&format!("Added {} {m}. Cloud is on.", provider.name)); }
                Err(e) => st.set_text(&format!("Could not save the account: {e}")),
            }
        });
        b.append(&btn);
    }
    let close = gtk::Button::with_label("Close"); close.set_halign(gtk::Align::Start);
    let (col, me) = (column.clone(), w.clone());
    close.connect_clicked(move |_| col.remove(&me));
    b.append(&close);
    w
}

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
                let mut ui_gone = false;
                while !pump.is_finished() {
                    match say_rx.recv_timeout(Duration::from_millis(200)) {
                        Ok(t) => { if writer.say(&t).is_err() { break } }
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

fn run(prog: &str, args: &[&str], key: Option<&str>) -> String {
    let mut c = std::process::Command::new(prog);
    c.args(args);
    // The key rides in the environment, never on a command line anyone can read with ps.
    if let Some(k) = key { c.env("AI_OS_MODEL_KEY", k); }
    c.output().map(|o| String::from_utf8_lossy(&o.stdout).into_owned()).unwrap_or_default()
}

fn engine_env() -> String { run("systemctl", &["--user", "show", "ai-os-engine.service", "-p", "Environment", "--value"], None) }

/// Makes the engine use a choice: the drop-in, the key file, a restart. The service restarts under
/// the rail, which reconnects on its own.
fn apply_model(kind: &str, url: &str, model: &str, key: Option<&str>) -> Result<(), String> {
    let text = aios_rail::models::dropin(kind, url, model).ok_or("that model's name or address has characters it may not")?;
    let home = std::env::var("HOME").map_err(|_| "no home folder")?;
    let dir = format!("{home}/.config/systemd/user/ai-os-engine.service.d");
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    std::fs::write(format!("{dir}/model.conf"), text).map_err(|e| e.to_string())?;
    let env_file = format!("{home}/.config/ai-os/model.env");
    match key {
        Some(k) => {
            use std::io::Write;
            use std::os::unix::fs::OpenOptionsExt;
            std::fs::create_dir_all(format!("{home}/.config/ai-os")).map_err(|e| e.to_string())?;
            let mut f = std::fs::OpenOptions::new().write(true).create(true).truncate(true).mode(0o600).open(&env_file).map_err(|e| e.to_string())?;
            writeln!(f, "AI_OS_MODEL_KEY={k}").map_err(|e| e.to_string())?;
        }
        None if kind == "ollama" => { let _ = std::fs::remove_file(&env_file); }
        None => {}
    }
    for args in [&["--user", "daemon-reload"][..], &["--user", "restart", "ai-os-engine.service"][..]] {
        let ok = std::process::Command::new("systemctl").args(args).status().map(|s| s.success()).unwrap_or(false);
        if !ok { return Err(format!("systemctl {} failed", args.join(" "))); }
    }
    Ok(())
}

/// A card with a title and a line of text, for the Model card and the key box.
fn plain_card(title: &str, text: &str) -> gtk::Box {
    let b = gtk::Box::new(gtk::Orientation::Vertical, 4);
    b.add_css_class("card"); b.add_css_class("ask");
    let t = gtk::Label::new(Some(title)); t.add_css_class("title"); t.set_xalign(0.0);
    let l = gtk::Label::new(Some(text)); l.set_xalign(0.0); l.set_wrap(true);
    b.append(&t); b.append(&l);
    b
}

/// The Model card: what the engine uses now, and a button per model the finder saw.
fn models_card(found: &str, env: &str, key: Option<String>, column: &gtk::Box, to_ui: &Sender<FromNet>, status: &gtk::Label) -> gtk::Widget {
    let now = match aios_rail::models::current(env) {
        (Some(m), Some(u)) => format!("Now using {m} at {u}."),
        (Some(m), None) => format!("Now using {m} on this machine."),
        _ => "No model set yet.".into(),
    };
    let choices = aios_rail::models::parse_found(found);
    let text = if choices.is_empty() { format!("{now} No models answered on your network.") } else { format!("{now} Pick one to switch; the AI restarts with it.") };
    let b = plain_card("Choose a model", &text);
    let w: gtk::Widget = b.clone().upcast();
    for c in choices {
        let btn = gtk::Button::with_label(&aios_rail::models::label(&c));
        btn.set_halign(gtk::Align::Start);
        let (col, me, tx, st, key) = (column.clone(), w.clone(), to_ui.clone(), status.clone(), key.clone());
        btn.connect_clicked(move |_| {
            col.remove(&me);
            match &c.model {
                Some(m) => st.set_text(&match apply_model(&c.kind, &c.url, m, key.as_deref()) {
                    Ok(()) => format!("Switched to {m}."),
                    Err(e) => format!("Could not switch: {e}"),
                }),
                None => {
                    // An LM Studio that wants its key: ask for it here, then list its models with it.
                    let k = plain_card("Its API key", &format!("LM Studio at {} needs its API key (LM Studio → Developer → Server settings).", c.url));
                    let entry = gtk::PasswordEntry::new();
                    entry.set_show_peek_icon(true);
                    let go = gtk::Button::with_label("Use this key");
                    go.set_halign(gtk::Align::Start);
                    k.append(&entry); k.append(&go);
                    let kw: gtk::Widget = k.upcast();
                    col.append(&kw);
                    let (col2, tx2, url) = (col.clone(), tx.clone(), c.url.clone());
                    go.connect_clicked(move |_| {
                        let key = entry.text().to_string();
                        if !aios_rail::models::safe_key(&key) { return; }
                        col2.remove(&kw);
                        let (tx3, url2) = (tx2.clone(), url.clone());
                        std::thread::spawn(move || {
                            let found = run("ai-os-find", &["--url", &url2], Some(&key));
                            let _ = tx3.send(FromNet::Keyed { url: url2, key, found });
                        });
                    });
                }
            }
        });
        b.append(&btn);
    }
    let accounts = gtk::Button::with_label("Cloud accounts…");
    accounts.set_halign(gtk::Align::Start);
    let (col, me, tx, st) = (column.clone(), w.clone(), to_ui.clone(), status.clone());
    accounts.connect_clicked(move |_| { col.remove(&me); col.append(&cloud_card(&col, &tx, &st)); });
    b.append(&accounts);
    let close = gtk::Button::with_label("Close");
    close.set_halign(gtk::Align::Start);
    let (col, me) = (column.clone(), w.clone());
    close.connect_clicked(move |_| col.remove(&me));
    b.append(&close);
    w
}

fn open_path(path: &str) {
    // Only paths the service sent reach here (cards.rs builds `opens` from events alone).
    let _ = std::process::Command::new("xdg-open").arg(path).stdout(std::process::Stdio::null()).stderr(std::process::Stdio::null()).spawn();
}

fn render(card: &Card, say: &Sender<String>, entry: &gtk::Entry) -> gtk::Widget {
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
                            let _ = s.send(t); e.set_text("");
                            if let Some(card) = w.ancestor(gtk::Box::static_type()) { card.set_sensitive(false); }
                        } else { e.set_text(&t); e.set_position(-1); e.grab_focus(); }
                    });
                    row.insert(&w, -1);
                }
                b.append(&row);
            }
        }
        CardKind::NeedsOk { what, why } => { b.add_css_class("ok"); b.append(&title("Needs your OK")); b.append(&text(what)); let l = text(why); l.add_css_class("dim"); b.append(&l); }
        CardKind::Done { text: t, check, files, windows } => {
            b.add_css_class("done"); b.append(&title("Done")); b.append(&text(t));
            if let Some(c) = check { let l = text(&format!("check: {c}")); l.add_css_class("dim"); b.append(&l); }
            if let Some(n) = aios_proto::window_note(windows) { let l = text(&n); l.add_css_class("dim"); b.append(&l); }
            file_rows(&b, files);
        }
        CardKind::Failed { text: t, files } | CardKind::Stopped { text: t, files } => {
            b.add_css_class("failed"); b.append(&title(if matches!(card.kind, CardKind::Failed { .. }) { "Could not finish" } else { "Stopped" })); b.append(&text(t));
            file_rows(&b, files);
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

/// The guide in a window of its own beside the chat, so the chat stays usable while it is open.
fn show_guide(parent: &gtk::ApplicationWindow) {
    let text = gtk::Label::builder().label(aios_rail::GUIDE).use_markup(true).wrap(true).xalign(0.0).selectable(true).focusable(false)
        .margin_start(16).margin_end(16).margin_top(12).margin_bottom(16).build();
    let scroll = gtk::ScrolledWindow::builder().child(&text).hscrollbar_policy(gtk::PolicyType::Never).build();
    let win = gtk::Window::builder().title("AI OS guide").transient_for(parent).default_width(520).default_height(600).child(&scroll).build();
    win.present();
}

fn main() {
    // The software renderer, unless the person chose one: GTK's GPU renderers left new cards
    // undrawn until a scroll in the owner's VirtualBox VM, and a column of text cards gains
    // nothing from the GPU on real hardware either.
    if std::env::var_os("GSK_RENDERER").is_none() { std::env::set_var("GSK_RENDERER", "cairo"); }
    let app = gtk::Application::builder().application_id("org.aios.Rail").build();
    app.connect_activate(|app| {
        let css = gtk::CssProvider::new(); css.load_from_string(CSS);
        gtk::style_context_add_provider_for_display(&gtk::gdk::Display::default().unwrap(), &css, gtk::STYLE_PROVIDER_PRIORITY_APPLICATION);
        let win = gtk::ApplicationWindow::builder().application(app).title("AI OS").default_width(420).default_height(600).build();
        // 600, not taller: a small screen (a VM's, a laptop's) put a taller window's bottom — the
        // newest card, its Yes button, the entry — below the edge. The cards scroll inside it.
        let header = gtk::HeaderBar::new();
        let clear = gtk::Button::with_label("Clear");
        clear.set_tooltip_text(Some("Clear the chat (a task still running stays)"));
        header.pack_start(&clear);
        let model_btn = gtk::Button::with_label("Model");
        model_btn.set_tooltip_text(Some("Switch the AI's model"));
        header.pack_start(&model_btn);
        // Stays on until turned off (the owner's call, 2026-09-19); the engine reads it every turn.
        let cloud = gtk::ToggleButton::with_label("Cloud");
        cloud.set_tooltip_text(Some("Use your cloud accounts: stronger models, but what you ask leaves this computer"));
        cloud.set_active(cloud_on());
        header.pack_start(&cloud);
        // Stop where it can always be reached while a job runs, not on a card scrolled out of view.
        let stop = gtk::Button::with_label("Stop");
        stop.add_css_class("destructive-action");
        stop.set_visible(false);
        header.pack_end(&stop);
        let help = gtk::Button::with_label("Help");
        help.set_tooltip_text(Some("How the AI OS works"));
        header.pack_end(&help);
        win.set_titlebar(Some(&header));
        let parent = win.clone();
        help.connect_clicked(move |_| show_guide(&parent));
        let column = gtk::Box::new(gtk::Orientation::Vertical, 0);
        let scroll = gtk::ScrolledWindow::builder().vexpand(true).child(&column).build();
        let status = gtk::Label::new(Some("Connecting to the AI OS service…")); status.add_css_class("status"); status.set_xalign(0.0);
        let entry = gtk::Entry::builder().placeholder_text("Tell the AI what you want…").margin_start(8).margin_end(8).margin_bottom(8).build();
        let root = gtk::Box::new(gtk::Orientation::Vertical, 4);
        // The spinner beside the status line: turning while the AI works (cards::busy_after).
        let spinner = gtk::Spinner::new(); spinner.set_margin_start(12);
        let status_row = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        status_row.append(&spinner); status_row.append(&status);
        root.append(&scroll); root.append(&status_row); root.append(&entry);
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

        let (tx_m, cards_m, status_m) = (to_ui_models, cards.clone(), status.clone());
        model_btn.connect_clicked(move |_| {
            if cards_m.borrow().running() { status_m.set_text("Finish or stop the task first, then switch models."); return; }
            status_m.set_text("Looking for models on your network…");
            let tx = tx_m.clone();
            std::thread::spawn(move || { let found = run("ai-os-find", &[], None); let _ = tx.send(FromNet::Models { found, env: engine_env() }); });
        });
        let (col_c, st_c) = (column.clone(), status.clone());
        cloud.connect_toggled(move |t| {
            let flag = format!("{}/cloud-on", config_dir());
            let done = if t.is_active() { write_private(&flag, "") } else { std::fs::remove_file(&flag).or_else(|e| if e.kind() == std::io::ErrorKind::NotFound { Ok(()) } else { Err(e) }).map_err(|e| e.to_string()) };
            if let Err(e) = done { st_c.set_text(&format!("Could not switch the cloud: {e}")); return }
            // Nothing to use yet: the card to add an account, rather than a switch that does nothing.
            if t.is_active() && aios_rail::models::accounts(&cloud_accounts()).is_empty() { col_c.append(&cloud_card(&col_c, &to_ui_cloud, &st_c)); }
            st_c.set_text(if t.is_active() { "Cloud is on: your cloud accounts answer first." } else { "Cloud is off: only your own models answer." });
        });
        let s_stop = say.clone();
        stop.connect_clicked(move |_| { let _ = s_stop.send("stop".into()); });
        let (cards3, widgets3, column3, say3, entry3) = (cards.clone(), widgets.clone(), column.clone(), say.clone(), entry.clone());
        clear.connect_clicked(move |_| {
            cards3.borrow_mut().clear();
            for w in widgets3.borrow_mut().drain(..) { column3.remove(&w); }
            for c in &cards3.borrow().list { let w = render(c, &say3, &entry3); column3.append(&w); widgets3.borrow_mut().push(w); }
        });

        let s = say.clone();
        entry.connect_activate(move |e| { let t = e.text().trim().to_string(); if !t.is_empty() { let _ = s.send(t); e.set_text(""); } });

        let (cards2, widgets2, column2, status2, say2, spinner2, stop2, entry2, cloud2) = (cards.clone(), widgets.clone(), column.clone(), status.clone(), say.clone(), spinner.clone(), stop.clone(), entry.clone(), cloud.clone());
        glib::timeout_add_local(Duration::from_millis(50), move || {
            while let Ok(msg) = from_net.try_recv() {
                match msg {
                    FromNet::Up => status2.set_text(""),
                    FromNet::Update(v) => column2.prepend(&update_card(&v, &column2)),
                    FromNet::Models { found, env } => {
                        status2.set_text("");
                        column2.append(&models_card(&found, &env, None, &column2, &to_ui2, &status2));
                    }
                    FromNet::Keyed { url, key, found } => {
                        // Only that runner's lines, and none still asking for a key: the key worked.
                        let only: String = found.lines().filter(|l| l.split('\t').nth(1) == Some(url.as_str())).map(|l| format!("{l}\n")).collect();
                        if only.is_empty() || only.contains("\t-\t") { status2.set_text("LM Studio did not accept that key."); }
                        else { column2.append(&models_card(&only, &engine_env(), Some(key), &column2, &to_ui2, &status2)); }
                    }
                    FromNet::Cloud { provider, key, found } => { status2.set_text(""); column2.append(&cloud_models_card(provider, &found, key, &column2, &status2, &cloud2)); }
                    FromNet::Down => { spinner2.stop(); status2.set_text("The AI OS service is not running — retrying…"); }
                    FromNet::Event(ev) => {
                        match aios_rail::cards::busy_after(&ev) {
                            Some(true) => { spinner2.start(); status2.set_text("The AI is thinking…"); }
                            Some(false) => { spinner2.stop(); status2.set_text(""); }
                            None => {}
                        }
                        // `apply` on its own line: the RefMut must end before `render` borrows.
                        let changes = cards2.borrow_mut().apply(&ev);
                        for ch in changes {
                            match ch {
                                Change::Added(i) => { let w = render(&cards2.borrow().list[i], &say2, &entry2); column2.append(&w); widgets2.borrow_mut().push(w); }
                                Change::Updated(i) => { let old = widgets2.borrow()[i].clone(); let w = render(&cards2.borrow().list[i], &say2, &entry2); column2.insert_child_after(&w, Some(&old)); column2.remove(&old); widgets2.borrow_mut()[i] = w; }
                                Change::Line(t) => status2.set_text(&t),
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
