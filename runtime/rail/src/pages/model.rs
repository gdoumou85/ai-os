//! Model and Cloud: the Model card and the cloud accounts, as a page.
use crate::FromNet;
use gtk4 as gtk;
use gtk::prelude::*;
use std::sync::mpsc::Sender;

/// Where the chat window and the engine's cloud pool share the switch and the accounts (core::cloud).
pub(crate) fn config_dir() -> String { format!("{}/.config/ai-os", std::env::var("HOME").unwrap_or_default()) }
pub(crate) fn cloud_on() -> bool { std::path::Path::new(&format!("{}/cloud-on", config_dir())).exists() }
pub(crate) fn cloud_accounts() -> String { std::fs::read_to_string(format!("{}/cloud.tsv", config_dir())).unwrap_or_default() }

/// A file only its owner can read: the keys are in it.
pub(crate) fn write_private(path: &str, text: &str) -> Result<(), String> {
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;
    std::fs::create_dir_all(config_dir()).map_err(|e| e.to_string())?;
    let mut f = std::fs::OpenOptions::new().write(true).create(true).truncate(true).mode(0o600).open(path).map_err(|e| e.to_string())?;
    f.write_all(text.as_bytes()).map_err(|e| e.to_string())
}

/// The cloud accounts card: each account with Remove, and a key box with a button per provider.
/// ponytail: NVIDIA, OpenRouter and Ollama; the other OpenAI-style providers (Groq, Mistral…) get a button
/// each once someone has a key to test them with.
pub(crate) fn cloud_card(column: &gtk::Box, to_ui: &Sender<FromNet>, status: &gtk::Label) -> gtk::Widget {
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
    b.append(&add); b.append(&entry); b.append(&row);
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
    w
}

/// The models a provider's key opens, a button each: a click adds the account and turns Cloud on.
pub(crate) fn cloud_models_card(provider: aios_rail::models::Provider, found: &str, key: String, column: &gtk::Box, status: &gtk::Label, switch: &gtk::ToggleButton) -> gtk::Widget {
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
    w
}

pub(crate) fn run(prog: &str, args: &[&str], key: Option<&str>) -> String {
    let mut c = std::process::Command::new(prog);
    c.args(args);
    // The key rides in the environment, never on a command line anyone can read with ps.
    if let Some(k) = key { c.env("AI_OS_MODEL_KEY", k); }
    c.output().map(|o| String::from_utf8_lossy(&o.stdout).into_owned()).unwrap_or_default()
}

pub(crate) fn engine_env() -> String { run("systemctl", &["--user", "show", "ai-os-engine.service", "-p", "Environment", "--value"], None) }

/// Makes the engine use a choice: the drop-in, the key file, a restart. The service restarts under
/// the rail, which reconnects on its own.
pub(crate) fn apply_model(kind: &str, url: &str, model: &str, key: Option<&str>, context: Option<usize>) -> Result<(), String> {
    let text = aios_rail::models::dropin(kind, url, model, context).ok_or("that model's name or address has characters it may not")?;
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
pub(crate) fn plain_card(title: &str, text: &str) -> gtk::Box {
    let b = gtk::Box::new(gtk::Orientation::Vertical, 4);
    b.add_css_class("card"); b.add_css_class("ask");
    let t = gtk::Label::new(Some(title)); t.add_css_class("title"); t.set_xalign(0.0);
    let l = gtk::Label::new(Some(text)); l.set_xalign(0.0); l.set_wrap(true);
    b.append(&t); b.append(&l);
    b
}

/// The Model card: what the engine uses now, and a button per model the finder saw.
pub(crate) fn models_card(found: &str, env: &str, key: Option<String>, column: &gtk::Box, to_ui: &Sender<FromNet>, status: &gtk::Label) -> gtk::Widget {
    let now = match aios_rail::models::current(env) {
        (Some(m), Some(u)) => format!("Now using {m} at {u}."),
        (Some(m), None) => format!("Now using {m} on this machine."),
        _ => "No model set yet.".into(),
    };
    let choices = aios_rail::models::parse_found(found);
    let held = aios_rail::models::env_value(env, "AI_OS_CONTEXT=").and_then(|v| aios_rail::models::safe_context(&v));
    let text = if choices.is_empty() { format!("{now} No models answered on your network.") } else { format!("{now} Pick one to switch; the AI restarts with it.") };
    let b = plain_card("Choose a model", &text);
    let w: gtk::Widget = b.clone().upcast();
    // How much the model can hold (the owner, 2026-09-21: a bar on the card, not a command).
    // LM Studio and the cloud runners are never told a context size, so the engine only ever
    // guessed 8192 -- and that guess is what caps `look` at 40 controls however big the model is.
    use aios_rail::models::{HOLDS, hold_index, hold_label};
    let bar = gtk::Scale::with_range(gtk::Orientation::Horizontal, 0.0, (HOLDS.len() - 1) as f64, 1.0);
    bar.set_round_digits(0);
    bar.set_draw_value(false);
    bar.set_hexpand(true);
    for (i, h) in HOLDS.iter().enumerate() {
        bar.add_mark(i as f64, gtk::PositionType::Bottom, Some(&hold_label(*h)));
    }
    bar.set_value(hold_index(held.unwrap_or(HOLDS[0])) as f64);
    for c in choices {
        let btn = gtk::Button::with_label(&aios_rail::models::label(&c));
        btn.set_halign(gtk::Align::Start);
        let (col, me, tx, st, key, bar) = (column.clone(), w.clone(), to_ui.clone(), status.clone(), key.clone(), bar.clone());
        btn.connect_clicked(move |_| {
            col.remove(&me);
            // The bar as it stands now: the owner moved it, then picked the model, and the
            // size he had set before came back instead (2026-09-23).
            let hold = Some(HOLDS[bar.value() as usize]);
            match &c.model {
                Some(m) => st.set_text(&match apply_model(&c.kind, &c.url, m, key.as_deref(), hold) {
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
    let chosen = gtk::Label::new(None);
    chosen.set_width_chars(10);
    let set = gtk::Button::with_label("Set");
    let show = {
        let chosen = chosen.clone();
        move |b: &gtk::Scale| chosen.set_text(&hold_label(HOLDS[b.value() as usize]))
    };
    show(&bar);
    bar.connect_value_changed(show);
    let row = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    row.append(&gtk::Label::new(Some("How much it can hold:")));
    row.append(&bar);
    row.append(&chosen);
    row.append(&set);
    b.append(&row);
    let hint = gtk::Label::new(Some("Move the bar to the context length you loaded the model with, \
then press Set. LM Studio shows it beside the model; a cloud model holds far more. Leave it at 8k if \
you are not sure -- set higher than the model really holds and its answers start coming back cut off. \
A full agent wants 16k or more when this PC can hold it: at 8k it remembers only the last few steps of the chat."));
    hint.set_xalign(0.0); hint.set_wrap(true); hint.add_css_class("dim");
    b.append(&hint);
    let (st, env_now, key_now) = (status.clone(), env.to_string(), key.clone());
    set.connect_clicked(move |_| {
        let n = HOLDS[bar.value() as usize];
        let Some((kind, url, model)) = aios_rail::models::current_choice(&env_now) else {
            st.set_text("Pick a model first, then say how much it can hold."); return;
        };
        st.set_text(&match apply_model(&kind, &url, &model, key_now.as_deref(), Some(n)) {
            Ok(()) => format!("It can hold {} now. The AI restarted with it.", hold_label(n)),
            Err(e) => format!("Could not set it: {e}"),
        });
    });
    let accounts = gtk::Button::with_label("Cloud accounts…");
    accounts.set_halign(gtk::Align::Start);
    let (col, me, tx, st) = (column.clone(), w.clone(), to_ui.clone(), status.clone());
    accounts.connect_clicked(move |_| { col.remove(&me); col.append(&cloud_card(&col, &tx, &st)); });
    b.append(&accounts);
    w
}
