//! Is there a newer release on GitHub than the one installed? Asked once per login, when the
//! rail opens (the owner's request, 2026-09-19): the answer is a card with Update now / Later.
//! No network, no answer: the check never gets in the way of the chat.

/// Where `get.sh` fetches from; the same repository the installer names.
pub const REPO: &str = "gdoumou85/ai-os";
/// What the installer writes, `git describe --tags` of the build (`v0.3.1`, or `v0.3.1-2-gabc` between tags).
pub const VERSION_FILE: &str = "/usr/local/share/ai-os/VERSION";

/// `v0.3.1` → (0, 3, 1). Anything after the three numbers (`-2-gabc`) is a build between tags,
/// counted as that tag.
pub fn parse(v: &str) -> Option<(u32, u32, u32)> {
    let mut parts = v.trim().trim_start_matches('v').splitn(3, '.');
    let major = parts.next()?.parse().ok()?;
    let minor = parts.next()?.parse().ok()?;
    let patch: String = parts.next()?.chars().take_while(char::is_ascii_digit).collect();
    Some((major, minor, patch.parse().ok()?))
}

/// True only when both read as versions and `latest` is the later one: a version that cannot be
/// read never nags the person to update.
pub fn newer(installed: &str, latest: &str) -> bool {
    matches!((parse(installed), parse(latest)), (Some(i), Some(l)) if l > i)
}

/// The tag at the end of the URL GitHub redirects `/releases/latest` to (`…/releases/tag/v0.3.1`).
pub fn tag_from_url(url: &str) -> Option<String> {
    let (_, tag) = url.trim().rsplit_once("/releases/tag/")?;
    (!tag.is_empty()).then(|| tag.to_string())
}

/// The newer version to offer, if there is one. `curl` follows the redirect and prints where it
/// landed; that needs no API token and no JSON.
pub fn check() -> Option<String> {
    let installed = std::fs::read_to_string(VERSION_FILE).ok()?;
    let out = std::process::Command::new("curl")
        .args(["-fsSL", "--max-time", "8", "-o", "/dev/null", "-w", "%{url_effective}", &format!("https://github.com/{REPO}/releases/latest")])
        .output().ok()?;
    let latest = tag_from_url(&String::from_utf8_lossy(&out.stdout))?;
    newer(&installed, &latest).then_some(latest)
}

/// The command a terminal runs for "Update now": the installer in update mode, which keeps the
/// model already chosen, then a pause so the person can read how it went.
pub fn update_command() -> String {
    format!("curl -fsSL https://raw.githubusercontent.com/{REPO}/master/install/get.sh | bash -s -- --update; \
             echo; read -rp 'Finished. Restart the computer for the new version, then press Enter to close this window. '")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn versions_compare_as_numbers_not_text() {
        assert!(newer("v0.9.0", "v0.10.0"));
        assert!(newer("v0.3.0\n", "v0.3.1"));
        assert!(newer("v0.3.1", "v1.0.0"));
        assert!(!newer("v0.3.1", "v0.3.1"));
        assert!(!newer("v0.4.0", "v0.3.9"));
        assert!(!newer("v0.3.1-2-gabc1234", "v0.3.1"), "a build after v0.3.1 is not older than it");
        assert!(newer("v0.3.1-2-gabc1234", "v0.3.2"));
        assert!(!newer("garbage", "v9.9.9"), "an unreadable install never nags");
        assert!(!newer("v0.3.1", ""));
    }

    #[test]
    fn the_tag_comes_from_the_redirected_url() {
        assert_eq!(tag_from_url("https://github.com/gdoumou85/ai-os/releases/tag/v0.3.1"), Some("v0.3.1".into()));
        assert_eq!(tag_from_url("https://github.com/gdoumou85/ai-os/releases/latest"), None, "no release yet: no redirect");
        assert_eq!(tag_from_url(""), None);
    }

    #[test]
    fn update_mode_keeps_the_model() {
        assert!(update_command().contains("bash -s -- --update"));
    }
}
