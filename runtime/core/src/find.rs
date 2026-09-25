//! Finds model runners on the home network (network-models spec §1): every address of each
//! /24 this machine is on is tried on Ollama's door and on LM Studio's, and whatever answers is asked for its
//! models. Used by the installer (through `ai-os-find`) and by the engine when its runner moves.
use std::net::{Ipv4Addr, SocketAddr, TcpStream, UdpSocket};
use std::time::Duration;

pub const OLLAMA_PORT: u16 = 11434;
pub const LMSTUDIO_PORT: u16 = 1234;

/// Which language a runner speaks: Ollama's `/api/chat`, or the OpenAI-style
/// `/v1/chat/completions` LM Studio serves.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Kind { Ollama, OpenAi }

impl Kind {
    pub fn as_str(self) -> &'static str { match self { Kind::Ollama => "ollama", Kind::OpenAi => "openai" } }
    pub fn parse(s: &str) -> Option<Kind> {
        match s { "ollama" => Some(Kind::Ollama), "openai" => Some(Kind::OpenAi), _ => None }
    }
}

/// One model at one runner. `model` is `None` when the runner wants a key it was not given.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct Found { pub url: String, pub kind: Kind, pub model: Option<String> }

/// 127.0.0.1, then the rest of every /24 this machine is on: a VM with a NAT adapter (the laptop
/// at 10.0.2.2) and a bridged one (the home network) needs both (the owner, 2026-09-19).
pub fn home_hosts() -> Vec<Ipv4Addr> {
    // `hostname -I` lists every address of every interface; the default route's address is added
    // in case it is missing (no `hostname`).
    let listed = std::process::Command::new("hostname").arg("-I").output().map(|o| String::from_utf8_lossy(&o.stdout).into_owned()).unwrap_or_default();
    let mut mine: Vec<Ipv4Addr> = listed.split_whitespace().filter_map(|a| a.parse().ok()).collect();
    // A UDP "connect" sends nothing; it only makes the OS pick the local address for that route.
    if let Ok(SocketAddr::V4(a)) = UdpSocket::bind("0.0.0.0:0").and_then(|s| { s.connect("192.0.2.1:9")?; s.local_addr() }) { mine.push(*a.ip()); }
    hosts_around(&mine)
}

/// 127.0.0.1 and the /24 around each private address, without this machine's own addresses (it is
/// already 127.0.0.1: listing it twice would list its models twice). A public address is never
/// swept: that is someone else's network.
/// ponytail: a /24 per interface; a wider sweep if bigger home networks matter.
pub fn hosts_around(mine: &[Ipv4Addr]) -> Vec<Ipv4Addr> {
    let mut hosts = vec![Ipv4Addr::LOCALHOST];
    let mut nets: Vec<[u8; 3]> = mine.iter().filter(|a| a.is_private()).map(|a| { let [x, y, z, _] = a.octets(); [x, y, z] }).collect();
    nets.sort(); nets.dedup();
    for [x, y, z] in nets {
        hosts.extend((1..=254).map(|d| Ipv4Addr::new(x, y, z, d)).filter(|h| !mine.contains(h)));
    }
    hosts
}

/// Every model on `hosts`, sorted so the installer's numbering is the same from run to run.
pub fn scan(hosts: &[Ipv4Addr], ollama_port: u16, openai_port: u16, key: Option<&str>) -> Vec<Found> {
    let doors: Vec<(Kind, SocketAddr)> = hosts.iter()
        .flat_map(|h| [(Kind::Ollama, SocketAddr::from((*h, ollama_port))), (Kind::OpenAi, SocketAddr::from((*h, openai_port)))])
        .collect();
    // Eight doors per thread: a door that never answers costs its thread the whole timeout.
    let open: Vec<(Kind, SocketAddr)> = std::thread::scope(|s| {
        let handles: Vec<_> = doors.chunks(8).map(|chunk| s.spawn(move || {
            chunk.iter().filter(|(_, a)| TcpStream::connect_timeout(a, Duration::from_millis(300)).is_ok()).copied().collect::<Vec<_>>()
        })).collect();
        handles.into_iter().flat_map(|h| h.join().unwrap_or_default()).collect()
    });
    let mut found: Vec<Found> = open.into_iter().flat_map(|(kind, a)| ask(kind, &format!("http://{a}"), key)).collect();
    found.sort();
    found.dedup();
    found
}

/// What the runner at `url` has, asked as `kind`. Empty when it is not that kind of runner.
pub fn ask(kind: Kind, url: &str, key: Option<&str>) -> Vec<Found> {
    let agent = ureq::AgentBuilder::new().timeout(Duration::from_secs(3)).build();
    let (address, list, field) = match kind { Kind::Ollama => (format!("{url}/api/tags"), "models", "name"), Kind::OpenAi => (format!("{}/models", aios_proto::v1(url)), "data", "id") };
    let mut req = agent.get(&address);
    if let Some(k) = key { req = req.set("Authorization", &format!("Bearer {k}")); }
    let body: serde_json::Value = match req.call() {
        Ok(r) => match r.into_json() { Ok(v) => v, Err(_) => return vec![] },
        Err(ureq::Error::Status(401, _)) if kind == Kind::OpenAi => return vec![Found { url: url.into(), kind, model: None }],
        Err(_) => return vec![],
    };
    body[list].as_array().map(|a| a.iter()
        .filter_map(|m| m[field].as_str())
        // Embedding models turn text into numbers; they cannot hold a conversation.
        .filter(|n| !n.contains("embed"))
        .map(|n| Found { url: url.into(), kind, model: Some(n.into()) })
        .collect()).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::{closed_port, json_response, serve};

    #[test]
    fn an_ollama_lists_its_chat_models() {
        let a = serve(json_response("200 OK", r#"{"models":[{"name":"qwen3.5:9b"},{"name":"nomic-embed-text:latest"}]}"#), 1);
        let url = format!("http://{a}");
        assert_eq!(ask(Kind::Ollama, &url, None), vec![Found { url, kind: Kind::Ollama, model: Some("qwen3.5:9b".into()) }]);
    }

    #[test]
    fn an_lm_studio_lists_its_models() {
        let a = serve(json_response("200 OK", r#"{"object":"list","data":[{"id":"bonsai-8b"},{"id":"text-embedding-nomic"}]}"#), 1);
        let url = format!("http://{a}");
        assert_eq!(ask(Kind::OpenAi, &url, Some("k")), vec![Found { url, kind: Kind::OpenAi, model: Some("bonsai-8b".into()) }]);
    }

    #[test]
    fn an_lm_studio_that_wants_a_key_says_so() {
        let a = serve(json_response("401 Unauthorized", r#"{"error":{"code":"invalid_api_key"}}"#), 1);
        let url = format!("http://{a}");
        assert_eq!(ask(Kind::OpenAi, &url, None), vec![Found { url, kind: Kind::OpenAi, model: None }]);
    }

    #[test]
    fn something_else_on_the_door_is_not_a_runner() {
        let a = serve("HTTP/1.1 200 OK\r\nContent-Length: 5\r\nConnection: close\r\n\r\nhello".into(), 1);
        assert!(ask(Kind::Ollama, &format!("http://{a}"), None).is_empty());
    }

    #[test]
    fn a_scan_knocks_then_asks_and_passes_closed_doors() {
        // Two connections: the knock, then the question.
        let a = serve(json_response("200 OK", r#"{"models":[{"name":"bonsai"}]}"#), 2);
        let got = scan(&[Ipv4Addr::LOCALHOST], a.port(), closed_port(), None);
        assert_eq!(got, vec![Found { url: format!("http://{a}"), kind: Kind::Ollama, model: Some("bonsai".into()) }]);
    }

    #[test]
    fn the_home_network_starts_with_this_machine() {
        let hosts = home_hosts();
        assert_eq!(hosts[0], Ipv4Addr::LOCALHOST);
        assert!(hosts.len() <= 1 + 253 * 8, "a /24 per interface at most");
    }

    #[test]
    fn every_private_network_is_swept_once() {
        let nat = Ipv4Addr::new(10, 0, 2, 15);
        let home = Ipv4Addr::new(192, 168, 1, 40);
        let hosts = hosts_around(&[nat, home, nat, Ipv4Addr::new(8, 8, 8, 8)]);
        assert_eq!(hosts[0], Ipv4Addr::LOCALHOST);
        assert_eq!(hosts.len(), 1 + 253 + 253, "two /24s, each without this machine, the public one skipped");
        assert!(hosts.contains(&Ipv4Addr::new(10, 0, 2, 2)), "the VM's host laptop");
        assert!(hosts.contains(&Ipv4Addr::new(192, 168, 1, 7)), "the second PC");
        assert!(!hosts.contains(&nat) && !hosts.contains(&home));
    }

    #[test]
    fn kinds_read_back_what_they_write() {
        for k in [Kind::Ollama, Kind::OpenAi] { assert_eq!(Kind::parse(k.as_str()), Some(k)); }
        assert_eq!(Kind::parse("lmstudio"), None);
    }
}
