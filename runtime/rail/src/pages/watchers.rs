//! Watchers: what the AI keeps an eye on, each with its reason and a switch (watchers design §3).
use aios_proto::{Request, WatcherView};
use gtk4 as gtk;
use gtk::prelude::*;
use std::sync::mpsc::Sender;

thread_local! {
    /// The watchers whose rows are open: every alert redraws the page, and an open row stays open.
    static OPEN: std::cell::RefCell<std::collections::HashSet<String>> = Default::default();
}

pub fn fill(col: &gtk::Box, watchers: &[WatcherView], say: &Sender<Request>) {
    super::empty(col);
    if watchers.is_empty() {
        col.append(&super::msg("Nothing is being watched. Ask the AI to keep an eye on something (\"warn me when a file appears in Downloads\") and it shows up here."));
        return;
    }
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs() as i64);
    for w in watchers {
        let card = gtk::Box::new(gtk::Orientation::Vertical, 2);
        card.add_css_class("card");
        let dot = gtk::Label::new(Some("●"));
        dot.add_css_class(aios_rail::sidebar::watcher_dot(w));
        let head = super::msg(&aios_rail::sidebar::watcher_line(w, now));
        head.set_hexpand(true);
        let row = gtk::Box::new(gtk::Orientation::Horizontal, 6);
        row.append(&dot); row.append(&head);
        // A click opens its reason and last alert, with its switches.
        let more = gtk::Expander::builder().label_widget(&row).expanded(OPEN.with(|o| o.borrow().contains(&w.name))).build();
        let n = w.name.clone();
        more.connect_expanded_notify(move |e| OPEN.with(|o| { let mut o = o.borrow_mut(); if e.is_expanded() { o.insert(n.clone()); } else { o.remove(&n); } }));
        let inside = gtk::Box::new(gtk::Orientation::Vertical, 4);
        inside.append(&super::msg(&format!("Why: {}", w.reason)));
        if !w.last_text.is_empty() { let l = super::msg(&format!("Last alert: {}", w.last_text)); l.add_css_class("dim"); inside.append(&l); }
        let buttons = gtk::Box::new(gtk::Orientation::Horizontal, 6);
        let pause = gtk::Button::with_label(if w.paused { "Resume" } else { "Pause" });
        let (s, n, p) = (say.clone(), w.name.clone(), !w.paused);
        pause.connect_clicked(move |_| { let _ = s.send(Request::WatcherPause { name: n.clone(), paused: p }); });
        let delete = gtk::Button::with_label("Delete");
        delete.add_css_class("destructive-action");
        let (s, n) = (say.clone(), w.name.clone());
        delete.connect_clicked(move |_| { let _ = s.send(Request::WatcherDelete { name: n.clone() }); });
        buttons.append(&pause); buttons.append(&delete);
        inside.append(&buttons);
        more.set_child(Some(&inside));
        card.append(&more);
        col.append(&card);
    }
}
