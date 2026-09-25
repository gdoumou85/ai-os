pub mod cards;
pub mod update;
pub mod models;
pub mod sidebar;

/// The Help window's text, in Pango markup: what the AI OS is, how to talk to it, the models and
/// runners it works with, and the accounts the cloud pool will need. Every feature a person can
/// see gets its lines here in the same change that adds it.
pub const GUIDE: &str = include_str!("guide.txt");

#[cfg(test)]
mod tests {
    #[test]
    fn the_guide_is_valid_markup() {
        // A stray `&` or `<` makes GTK show the guide as raw tags. `<a href>` is the label's own,
        // not Pango's: taken out before Pango reads the rest.
        let mut plain = super::GUIDE.replace("</a>", "");
        while let Some(i) = plain.find("<a href=") {
            let end = i + plain[i..].find('>').expect("an <a> tag is closed");
            plain.replace_range(i..=end, "");
        }
        gtk4::pango::parse_markup(&plain, '\0').expect("guide.txt is not valid Pango markup");
    }
}
