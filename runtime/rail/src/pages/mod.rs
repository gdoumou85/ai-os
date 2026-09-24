//! The sidebar's pages (sidebar design §3): the menu entries and what each page shows.
pub mod help;
pub mod model;
pub mod projects;
pub mod skills;
pub mod watchers;

use gtk4 as gtk;
use gtk::prelude::*;

/// A menu entry: a flat toggle in one group, so exactly one is pressed; pressing it shows `page`.
pub fn nav(label: &str, page: &'static str, stack: &gtk::Stack, group: Option<&gtk::ToggleButton>) -> gtk::ToggleButton {
    let b = gtk::ToggleButton::with_label(label);
    b.add_css_class("flat"); b.add_css_class("nav");
    if let Some(l) = b.child().and_downcast::<gtk::Label>() { l.set_xalign(0.0); }
    if let Some(g) = group { b.set_group(Some(g)); }
    let s = stack.clone();
    b.connect_toggled(move |b| if b.is_active() { s.set_visible_child_name(page) });
    b
}

/// A page's column of cards.
pub fn page_column() -> gtk::Box {
    let c = gtk::Box::new(gtk::Orientation::Vertical, 6);
    c.set_margin_start(8); c.set_margin_end(8); c.set_margin_top(8); c.set_margin_bottom(8);
    c
}

pub fn scrolled(child: &impl IsA<gtk::Widget>) -> gtk::ScrolledWindow {
    gtk::ScrolledWindow::builder().child(child).hscrollbar_policy(gtk::PolicyType::Never).vexpand(true).build()
}

/// Everything off a page, before it is drawn again.
pub fn empty(col: &gtk::Box) {
    while let Some(c) = col.first_child() { col.remove(&c); }
}

/// A line of wrapped text left on a page once a card closes, so the page is never blank.
pub fn msg(text: &str) -> gtk::Label {
    let l = gtk::Label::new(Some(text));
    l.set_wrap(true); l.set_xalign(0.0);
    l
}
