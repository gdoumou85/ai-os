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

/// How much the model can hold at once, as the Model card takes it: a plain number of tokens.
/// Under 8192 the standing rules alone crowd out the work; over a million is a typo.
pub fn safe_context(s: &str) -> Option<usize> {
    s.trim().parse().ok().filter(|n| (8192..=1_000_000).contains(n))
}

/// The drop-in that makes the engine use a choice. It is applied after the installer's unit, so
/// its lines win. `context` is what the runner loaded the model with (the owner, 2026-09-21: he
/// will not set it from a command line); left out, the engine keeps its own 8192.
pub fn dropin(kind: &str, url: &str, model: &str, context: Option<usize>) -> Option<String> {
    if !(matches!(kind, "ollama" | "openai") && safe(url) && safe(model)) { return None }
    let mut t = format!(
        "# Written by the AI OS chat window's Model button.\n[Service]\nEnvironment=AI_OS_MODEL={model}\nEnvironment=AI_OS_MODEL_URL={url}\nEnvironment=AI_OS_MODEL_KIND={kind}\n");
    if let Some(n) = context { t.push_str(&format!("Environment=AI_OS_CONTEXT={n}\n")); }
    Some(t)
}

/// A key for the private `model.env`: printable, no spaces, one line.
pub fn safe_key(k: &str) -> bool { !k.is_empty() && k.len() <= 500 && k.chars().all(|c| c.is_ascii_graphic()) }

/// One value out of `systemctl --user show -p Environment --value`.
pub fn env_value(env: &str, key: &str) -> Option<String> {
    env.split_whitespace().filter_map(|kv| kv.strip_prefix(key)).last().map(String::from)
}

/// The model and address the engine runs with.
pub fn current(env: &str) -> (Option<String>, Option<String>) {
    (env_value(env, "AI_OS_MODEL="), env_value(env, "AI_OS_MODEL_URL="))
}

/// The whole choice the engine runs now, so the Model card can re-apply it with a new context.
pub fn current_choice(env: &str) -> Option<(String, String, String)> {
    Some((env_value(env, "AI_OS_MODEL_KIND=")?, env_value(env, "AI_OS_MODEL_URL=")?, env_value(env, "AI_OS_MODEL=")?))
}

/// The label on a choice's button.
pub fn label(c: &Choice) -> String {
    let who = if c.kind == "openai" { "LM Studio" } else { "Ollama" };
    match &c.model { Some(m) => format!("{m} — {who} at {}", c.url), None => format!("{who} at {} (needs its API key)", c.url) }
}

/// The line `cloud.tsv` (the engine's cloud pool) keeps for one account; `None` unless every part
/// is safe to write, so no tab or newline can make a second account of its own.
pub fn account_line(name: &str, kind: &str, url: &str, model: &str, key: &str) -> Option<String> {
    (safe(name) && matches!(kind, "ollama" | "openai") && safe(url) && safe(model) && safe_key(key)).then(|| format!("{name}\t{kind}\t{url}\t{model}\t{key}\n"))
}

/// The accounts in `cloud.tsv`, as the name and model to show; the keys stay in the file.
pub fn accounts(tsv: &str) -> Vec<(String, String)> {
    tsv.lines().filter_map(|l| { let f: Vec<&str> = l.split('\t').collect(); (f.len() == 5).then(|| (f[0].to_string(), f[3].to_string())) }).collect()
}

/// A cloud provider the Cloud card can add an account for: where it answers and how it speaks.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Provider { pub name: &'static str, pub kind: &'static str, pub url: &'static str }

/// NVIDIA's hosted models (build.nvidia.com): free credits, OpenAI-style. The owner's pick,
/// 2026-09-19, once Ollama's cloud stopped being free.
pub const NVIDIA: Provider = Provider { name: "NVIDIA", kind: "openai", url: "https://integrate.api.nvidia.com" };
pub const OLLAMA: Provider = Provider { name: "Ollama", kind: "ollama", url: "https://ollama.com" };
/// OpenRouter: many providers behind one key, OpenAI-style under `/api` (so `/api/v1/…`).
pub const OPENROUTER: Provider = Provider { name: "OpenRouter", kind: "openai", url: "https://openrouter.ai/api" };

/// The models worth offering from a provider's list. NVIDIA lists everything it hosts —
/// embedders, safety filters, picture readers — and only some of those can hold a conversation.
/// OpenRouter lists hundreds, most of them paid: only the free ones (`…:free`) are offered.
/// ponytail: a name filter; NVIDIA's own model types if its list ever says them.
pub fn chat_models(provider: Provider, names: Vec<String>) -> Vec<String> {
    if provider == OPENROUTER { return names.into_iter().filter(|n| n.ends_with(":free")).collect(); }
    if provider != NVIDIA { return names; }
    const NOT_CHAT: [&str; 15] = ["embed", "rerank", "retriev", "guard", "safety", "reward", "vision", "-vl", "vlm", "clip", "parse", "detect", "translat", "pii", "content-"];
    const CHAT: [&str; 9] = ["instruct", "chat", "-it", "deepseek", "kimi", "qwen3", "gpt-oss", "nemotron", "coder"];
    names.into_iter().filter(|n| {
        let l = n.to_lowercase();
        !NOT_CHAT.iter().any(|w| l.contains(w)) && CHAT.iter().any(|w| l.contains(w))
    }).collect()
}

/// `cloud.tsv` without its `i`th account.
pub fn without(tsv: &str, i: usize) -> String {
    tsv.lines().filter(|l| l.split('\t').count() == 5).enumerate().filter(|(n, _)| *n != i).map(|(_, l)| format!("{l}\n")).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cloud_accounts_are_written_read_and_removed_without_their_keys_showing() {
        let l = account_line("Ollama", "ollama", "https://ollama.com", "gpt-oss:120b", "abc.123").unwrap();
        assert_eq!(l, "Ollama\tollama\thttps://ollama.com\tgpt-oss:120b\tabc.123\n");
        assert_eq!(account_line("Ollama", "ollama", "https://ollama.com", "m", "k\tX\tollama\tu\tm"), None, "a key with a tab is refused");
        let tsv = format!("{l}{}", account_line("Groq", "openai", "https://api.groq.com/openai", "llama", "k2").unwrap());
        assert_eq!(accounts(&tsv), vec![("Ollama".into(), "gpt-oss:120b".into()), ("Groq".into(), "llama".into())]);
        assert_eq!(accounts(&without(&tsv, 0)), vec![("Groq".into(), "llama".into())]);
    }

    #[test]
    fn nvidias_list_is_cut_to_the_models_that_can_talk() {
        let names = ["meta/llama-3.3-70b-instruct", "nvidia/nv-embedqa-e5-v5", "nvidia/llama-3.1-nemoguard-8b-content-safety",
            "deepseek-ai/deepseek-v3.1", "microsoft/phi-3.5-vision-instruct", "qwen/qwen3-coder-480b-a35b-instruct", "nvidia/nv-rerankqa-mistral-4b-v3",
            "google/gemma-2-27b-it", "openai/gpt-oss-120b"].map(String::from).to_vec();
        assert_eq!(chat_models(NVIDIA, names.clone()), vec!["meta/llama-3.3-70b-instruct", "deepseek-ai/deepseek-v3.1",
            "qwen/qwen3-coder-480b-a35b-instruct", "google/gemma-2-27b-it", "openai/gpt-oss-120b"]);
        assert_eq!(chat_models(OLLAMA, names.clone()), names, "Ollama's list is already its chat models");
        let or = ["nvidia/nemotron-nano-9b-v2:free", "openai/gpt-4o", "qwen/qwen3-coder:free"].map(String::from).to_vec();
        assert_eq!(chat_models(OPENROUTER, or), vec!["nvidia/nemotron-nano-9b-v2:free", "qwen/qwen3-coder:free"], "free ones only");
        assert!(account_line(OPENROUTER.name, OPENROUTER.kind, OPENROUTER.url, "nvidia/nemotron-nano-9b-v2:free", "sk-or-v1-abc123").is_some());
        assert!(account_line(NVIDIA.name, NVIDIA.kind, NVIDIA.url, "meta/llama-3.3-70b-instruct", "nvapi-abc_DEF-123").is_some(), "NVIDIA's names and keys are writable");
    }

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
    fn how_much_it_can_hold_is_a_plain_number_and_optional() {
        let d = dropin("openai", "http://x:1", "m", Some(32768)).unwrap();
        assert!(d.contains("Environment=AI_OS_CONTEXT=32768\n"));
        assert!(!dropin("openai", "http://x:1", "m", None).unwrap().contains("AI_OS_CONTEXT"));
        assert_eq!(safe_context(" 32768 "), Some(32768));
        // Nothing that could land in the unit file as anything but a number, and nothing so
        // small the standing rules alone would fill it.
        for bad in ["", "lots", "4096", "0", "-1", "2000000", "8192 ExecStart=/bin/sh"] {
            assert_eq!(safe_context(bad), None, "{bad}");
        }
    }

    #[test]
    fn the_engines_own_choice_comes_back_out_of_its_environment() {
        let env = "AI_OS_MODEL=m AI_OS_MODEL_URL=http://x:1 AI_OS_MODEL_KIND=openai AI_OS_CONTEXT=32768";
        assert_eq!(current_choice(env), Some(("openai".into(), "http://x:1".into(), "m".into())));
        assert_eq!(env_value(env, "AI_OS_CONTEXT=").as_deref(), Some("32768"));
        assert_eq!(current_choice("AI_OS_MODEL=m"), None, "half a choice is no choice");
    }

    #[test]
    fn the_dropin_carries_only_safe_values() {
        let d = dropin("openai", "http://10.0.2.2:1234", "bonsai-8b@q4_k_m", None).unwrap();
        assert!(d.contains("Environment=AI_OS_MODEL=bonsai-8b@q4_k_m\n"));
        assert!(d.contains("Environment=AI_OS_MODEL_KIND=openai\n"));
        assert_eq!(dropin("ollama", "http://x:1", "m\nExecStart=/bin/sh", None), None);
        assert_eq!(dropin("ollama", "http://x:1", "100%", None), None, "% is a systemd specifier");
        assert_eq!(dropin("other", "http://x:1", "m", None), None);
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
