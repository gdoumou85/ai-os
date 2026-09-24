//! Skills (Phase 3 §6): each notebook, its entries with how often they helped, and a ✕ that
//! deletes one. Rebuilt from every `skills` answer, so a delete shows at once.
use aios_proto::{Notebook, Request};
use gtk4 as gtk;
use gtk::prelude::*;
use std::sync::mpsc::Sender;

pub fn fill(col: &gtk::Box, notebooks: &[Notebook], say: &Sender<Request>) {
    super::empty(col);
    let column = gtk::Box::new(gtk::Orientation::Vertical, 6);
    column.set_margin_start(12); column.set_margin_end(12); column.set_margin_top(12); column.set_margin_bottom(12);
    // The first notebook is what is installed (machine-map spec §4), there even before any lesson.
    if notebooks.iter().all(|nb| nb.name == "installed on this computer") {
        let l = gtk::Label::new(Some("Nothing learned yet. After a job that worked, what the AI learned shows here."));
        l.set_wrap(true); l.set_xalign(0.0); col.append(&l);
    }
    for nb in notebooks {
        let list = gtk::Box::new(gtk::Orientation::Vertical, 4);
        for n in &nb.entries {
            let row = gtk::Box::new(gtk::Orientation::Horizontal, 6);
            // A program is what is on disk: nothing to forget, nothing used.
            if n.kind == "program" {
                let l = gtk::Label::new(Some(&format!("{}: {}", n.topic, n.text)));
                l.set_wrap(true); l.set_xalign(0.0); l.set_hexpand(true); l.set_selectable(true);
                row.append(&l);
                list.append(&row);
                continue;
            }
            let mark = if n.failed { " — did not work last time" } else if n.needs_check { " — needs checking" } else { "" };
            let l = gtk::Label::new(Some(&format!("{}: {}\nused {} time{}{mark}", n.topic, n.text, n.uses, if n.uses == 1 { "" } else { "s" })));
            l.set_wrap(true); l.set_xalign(0.0); l.set_hexpand(true); l.set_selectable(true);
            row.append(&l);
            let x = gtk::Button::with_label("✕");
            x.set_tooltip_text(Some("Delete this"));
            let (s, notebook, topic) = (say.clone(), nb.name.clone(), n.topic.clone());
            x.connect_clicked(move |_| { let _ = s.send(Request::Forget { notebook: notebook.clone(), topic: topic.clone() }); });
            row.append(&x);
            list.append(&row);
        }
        let title = match nb.name.as_str() { "this computer" => "This computer".to_string(), "installed on this computer" => "Installed on this computer".to_string(), n => n.to_string() };
        let ex = gtk::Expander::builder().label(format!("{title} ({})", nb.entries.len())).child(&list).expanded(notebooks.len() == 1).build();
        col.append(&ex);
    }
}
