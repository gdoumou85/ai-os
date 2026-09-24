//! Help: the guide, as a page.
use gtk4 as gtk;

pub fn page() -> gtk::ScrolledWindow {
    let text = gtk::Label::builder().label(aios_rail::GUIDE).use_markup(true).wrap(true).xalign(0.0).selectable(true).focusable(false)
        .margin_start(16).margin_end(16).margin_top(12).margin_bottom(16).build();
    super::scrolled(&text)
}
