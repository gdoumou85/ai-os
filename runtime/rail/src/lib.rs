pub mod cards;
pub mod update;
pub mod models;

/// The Help window's text, in Pango markup: what the AI OS is, how to talk to it, the models and
/// runners it works with, and the accounts the cloud pool will need. Every feature a person can
/// see gets its lines here in the same change that adds it.
pub const GUIDE: &str = include_str!("guide.txt");

#[cfg(test)]
mod tests {
    #[test]
    fn the_guide_is_valid_markup() {
        // A stray `&` or `<` makes GTK show the guide as raw tags.
        gtk4::pango::parse_markup(super::GUIDE, '\0').expect("guide.txt is not valid Pango markup");
    }
}
