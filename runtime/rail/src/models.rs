//! Switching models from the chat window (the owner's request, 2026-09-19): the finder's list as
//! buttons, and the choice written to a systemd drop-in of its own, so the installer's unit and
//! an update leave it alone. Pure parts here; main.rs runs the programs.

/// One line of `ai-os-find`: kind, url, model — `model` is `None` when the runner wants a key.
#[derive(Debug, Clone, PartialEq)]
pub struct Choice { pub kind: String, pub url: String, pub model: Option<String> }

/// Names and addresses come off the network, and they are written into a unit file: only
/// characters a model name or a URL needs, so no newline, `%` or space can add a directive of
/// its own. `@` and `+` are in LM Studio's ids; `:` and `/` in Ollama's.
pub fn safe(s: &str) -> bool {
    !s.is_empty() && s.len() <= 200 && s.chars().all(|c| c.is_ascii_alphanumeric() || "._:/@+-".contains(c))
}

/// `ai-os-find`'s lines as choices; a line that is not safe to write anywhere is dropped.
pub fn parse_found(out: &str) -> Vec<Choice> {
    out.lines().filter_map(|l| {
        let mut f = l.split('\t');
        let (kind, url, model) = (f.next()?, f.next()?, f.next()?);
        if !matches!(kind, "ollama" | "openai") || !safe(url) { return None; }
        let model = if model == "-" { None } else if safe(model) { Some(model.to_string()) } else { return None };
        Some(Choice { kind: kind.into(), url: url.into(), model })
    }).collect()
}

/// The drop-in that makes the engine use a choice. It is applied after the installer's unit, so
/// its lines win.
pub fn dropin(kind: &str, url: &str, model: &str) -> Option<String> {
    (matches!(kind, "ollama" | "openai") && safe(url) && safe(model)).then(|| format!(
        "# Written by the AI OS chat window's Model button.\n[Service]\nEnvironment=AI_OS_MODEL={model}\nEnvironment=AI_OS_MODEL_URL={url}\nEnvironment=AI_OS_MODEL_KIND={kind}\n"))
}

/// A key for the private `model.env`: printable, no spaces, one line.
pub fn safe_key(k: &str) -> bool { !k.is_empty() && k.len() <= 500 && k.chars().all(|c| c.is_ascii_graphic()) }

/// The model and address the engine runs with, from `systemctl --user show -p Environment --value`.
pub fn current(env: &str) -> (Option<String>, Option<String>) {
    let get = |k: &str| env.split_whitespace().filter_map(|kv| kv.strip_prefix(k)).last().map(String::from);
    (get("AI_OS_MODEL="), get("AI_OS_MODEL_URL="))
}

/// The label on a choice's button.
pub fn label(c: &Choice) -> String {
    let who = if c.kind == "openai" { "LM Studio" } else { "Ollama" };
    match &c.model { Some(m) => format!("{m} — {who} at {}", c.url), None => format!("{who} at {} (needs its API key)", c.url) }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn found_lines_become_choices_and_unsafe_ones_are_dropped() {
        let out = "ollama\thttp://10.0.2.2:11434\tMichelRosselli/bonsai-27b:latest\n\
                   openai\thttp://10.0.2.2:1234\t-\tneeds-key\n\
                   openai\thttp://10.0.2.2:1234\tevil\nExecStart=/bin/sh\n\
                   ollama\thttp://x:1\tbad name\n";
        let c = parse_found(out);
        assert_eq!(c, vec![
            Choice { kind: "ollama".into(), url: "http://10.0.2.2:11434".into(), model: Some("MichelRosselli/bonsai-27b:latest".into()) },
            Choice { kind: "openai".into(), url: "http://10.0.2.2:1234".into(), model: None },
            Choice { kind: "openai".into(), url: "http://10.0.2.2:1234".into(), model: Some("evil".into()) },
        ], "the injected line is not a line of the finder's, and a name with a space is refused");
    }

    #[test]
    fn the_dropin_carries_only_safe_values() {
        let d = dropin("openai", "http://10.0.2.2:1234", "bonsai-8b@q4_k_m").unwrap();
        assert!(d.contains("Environment=AI_OS_MODEL=bonsai-8b@q4_k_m\n"));
        assert!(d.contains("Environment=AI_OS_MODEL_KIND=openai\n"));
        assert_eq!(dropin("ollama", "http://x:1", "m\nExecStart=/bin/sh"), None);
        assert_eq!(dropin("ollama", "http://x:1", "100%"), None, "% is a systemd specifier");
        assert_eq!(dropin("other", "http://x:1", "m"), None);
    }

    #[test]
    fn keys_are_one_printable_line() {
        assert!(safe_key("sk-lm-AbC123:xyz"));
        assert!(!safe_key("two words"));
        assert!(!safe_key("a\nAI_OS_MODEL=x"));
        assert!(!safe_key(""));
    }

    #[test]
    fn the_current_model_is_the_last_setting_the_engine_sees() {
        let env = "AI_OS_DB=/data/ai-os.db AI_OS_MODEL=qwen3.5:9b AI_OS_MODEL_URL=http://a:1 AI_OS_MODEL=bonsai AI_OS_MODEL_URL=http://b:2";
        assert_eq!(current(env), (Some("bonsai".into()), Some("http://b:2".into())));
        assert_eq!(current(""), (None, None));
    }
}
