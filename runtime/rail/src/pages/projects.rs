//! Projects: every project the AI knows, newest work first (sidebar design §3).
use aios_proto::ProjectView;
use gtk4 as gtk;
use gtk::prelude::*;

pub fn fill(col: &gtk::Box, projects: &[ProjectView], entry: &gtk::Entry, chat: &gtk::ToggleButton) {
    super::empty(col);
    if projects.is_empty() {
        let l = gtk::Label::new(Some("No projects yet. Ask the AI to start one, and it shows up here."));
        l.set_wrap(true); l.set_xalign(0.0); col.append(&l);
        return;
    }
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs() as i64);
    for p in projects {
        let card = gtk::Box::new(gtk::Orientation::Vertical, 2);
        card.add_css_class("card");
        let name = gtk::Button::with_label(&p.name);
        name.add_css_class("flat"); name.add_css_class("title"); name.set_halign(gtk::Align::Start);
        name.set_tooltip_text(Some("Carry on with this project in the chat"));
        let (e, c, n) = (entry.clone(), chat.clone(), p.name.clone());
        name.connect_clicked(move |_| { e.set_text(&aios_rail::sidebar::continue_text(&n)); c.set_active(true); e.grab_focus(); e.set_position(-1); });
        card.append(&name);
        if !p.summary.is_empty() { let l = gtk::Label::new(Some(&p.summary)); l.set_wrap(true); l.set_xalign(0.0); card.append(&l); }
        let where_ = gtk::Label::new(Some(&format!("{} · changed {}", p.folder, aios_rail::sidebar::ago(now, p.touched_at))));
        where_.set_wrap(true); where_.set_xalign(0.0); where_.add_css_class("dim"); card.append(&where_);
        let open = gtk::Button::with_label("Open folder"); open.set_halign(gtk::Align::Start);
        let folder = p.folder.clone();
        open.connect_clicked(move |_| crate::open_path(&folder));
        card.append(&open);
        col.append(&card);
    }
}
