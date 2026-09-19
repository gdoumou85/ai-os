//! Skills (Phase 3 spec §1): notebooks of one-line entries, "this computer" plus one per craft.
//! One row per (notebook, topic): a better way replaces the line, it never piles on.
use crate::store::StoreError;
use rusqlite::{params, Connection, OptionalExtension};
use std::time::{SystemTime, UNIX_EPOCH};

pub const THIS_COMPUTER: &str = "this computer";
pub const MAX_ENTRIES: i64 = 200;
const MAX_TEXT: usize = 200;
const MAX_STEPS: usize = 400;

#[derive(Debug, Clone, PartialEq, Default)]
pub struct Note {
    pub notebook: String, pub topic: String, pub kind: String, pub text: String, pub steps: String,
    pub links: Vec<String>, pub uses: i64, pub failed: bool, pub needs_check: bool,
}

fn now_ms() -> i64 { SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_millis() as i64).unwrap_or(0) }
fn cut(s: &str, n: usize) -> String { s.chars().take(n).collect() }

pub fn norm(s: &str) -> String { s.split_whitespace().collect::<Vec<_>>().join(" ").to_lowercase() }
pub fn norm_notebook(s: &str) -> String { norm(&s.replace('/', " ")) }
pub fn key(notebook: &str, topic: &str) -> String { format!("{}/{}", norm_notebook(notebook), norm(topic)) }
fn split_key(k: &str) -> Option<(String, String)> {
    let (nb, tp) = k.split_once('/')?;
    Some((norm_notebook(nb), norm(tp)))
}

pub fn init(c: &Connection) -> Result<(), StoreError> {
    c.execute_batch(
        "CREATE TABLE IF NOT EXISTS notes(
           notebook TEXT NOT NULL, topic TEXT NOT NULL, kind TEXT NOT NULL, text TEXT NOT NULL,
           steps TEXT NOT NULL DEFAULT '', links TEXT NOT NULL DEFAULT '[]',
           uses INTEGER NOT NULL DEFAULT 0, failed INTEGER NOT NULL DEFAULT 0,
           needs_check INTEGER NOT NULL DEFAULT 0, pending_job TEXT, updated_at INTEGER NOT NULL,
           PRIMARY KEY(notebook, topic));",
    )?;
    Ok(())
}

const COLS: &str = "notebook, topic, kind, text, steps, links, uses, failed, needs_check";

fn row(r: &rusqlite::Row) -> rusqlite::Result<Note> {
    let links: String = r.get(5)?;
    Ok(Note {
        notebook: r.get(0)?, topic: r.get(1)?, kind: r.get(2)?, text: r.get(3)?, steps: r.get(4)?,
        links: serde_json::from_str(&links).unwrap_or_default(),
        uses: r.get(6)?, failed: r.get::<_, i64>(7)? != 0, needs_check: r.get::<_, i64>(8)? != 0,
    })
}

/// Add or replace by (notebook, topic): the use count stays, the failed/needs-check marks go.
/// Then the notebook is cut back to MAX_ENTRIES, least-used and oldest first — never the entry
/// just written, which starts with no uses and would otherwise be the first to go.
pub fn put(c: &Connection, n: &Note, pending_job: Option<&str>) -> Result<(), StoreError> {
    let (nb, tp) = (norm_notebook(&n.notebook), norm(&n.topic));
    let links = serde_json::to_string(&n.links.iter().map(|l| norm(l)).collect::<Vec<_>>())?;
    c.execute(
        "INSERT INTO notes(notebook, topic, kind, text, steps, links, uses, failed, needs_check, pending_job, updated_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, 0, 0, 0, ?7, ?8)
         ON CONFLICT(notebook, topic) DO UPDATE SET kind = excluded.kind, text = excluded.text, steps = excluded.steps,
           links = excluded.links, failed = 0, needs_check = 0, pending_job = excluded.pending_job, updated_at = excluded.updated_at",
        params![nb, tp, n.kind, cut(&n.text, MAX_TEXT), cut(&n.steps, MAX_STEPS), links, pending_job, now_ms()],
    )?;
    c.execute(
        "DELETE FROM notes WHERE rowid IN (SELECT rowid FROM notes WHERE notebook = ?1 AND topic != ?2 AND pending_job IS NULL
           ORDER BY uses DESC, updated_at DESC, rowid DESC LIMIT -1 OFFSET ?3)",
        params![nb, tp, MAX_ENTRIES - 1],
    )?;
    Ok(())
}

pub fn get(c: &Connection, notebook: &str, topic: &str) -> Result<Option<Note>, StoreError> {
    Ok(c.query_row(&format!("SELECT {COLS} FROM notes WHERE notebook = ?1 AND topic = ?2 AND pending_job IS NULL"),
        params![norm_notebook(notebook), norm(topic)], row).optional()?)
}

pub fn list(c: &Connection, notebook: &str) -> Result<Vec<Note>, StoreError> {
    let mut st = c.prepare(&format!("SELECT {COLS} FROM notes WHERE notebook = ?1 AND pending_job IS NULL ORDER BY uses DESC, updated_at DESC, rowid DESC"))?;
    let rows = st.query_map([norm_notebook(notebook)], row)?;
    Ok(rows.collect::<Result<_, _>>()?)
}

pub fn notebooks(c: &Connection) -> Result<Vec<String>, StoreError> {
    let mut st = c.prepare("SELECT DISTINCT notebook FROM notes WHERE pending_job IS NULL ORDER BY notebook")?;
    let mut names: Vec<String> = st.query_map([], |r| r.get(0))?.collect::<Result<_, _>>()?;
    if let Some(i) = names.iter().position(|n| n == THIS_COMPUTER) { let t = names.remove(i); names.insert(0, t); }
    Ok(names)
}

pub fn all(c: &Connection) -> Result<Vec<(String, Vec<Note>)>, StoreError> {
    notebooks(c)?.into_iter().map(|nb| { let l = list(c, &nb)?; Ok((nb, l)) }).collect()
}

/// Delete one entry. A "this computer" entry that goes marks every skill entry pointing to it.
pub fn remove(c: &Connection, notebook: &str, topic: &str) -> Result<bool, StoreError> {
    let (nb, tp) = (norm_notebook(notebook), norm(topic));
    let gone = c.execute("DELETE FROM notes WHERE notebook = ?1 AND topic = ?2", params![nb, tp])? > 0;
    if gone && nb == THIS_COMPUTER {
        let mut st = c.prepare("SELECT notebook, topic, links FROM notes WHERE notebook != ?1")?;
        let linking: Vec<(String, String)> = st.query_map([THIS_COMPUTER], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?, r.get::<_, String>(2)?)))?
            .filter_map(Result::ok)
            .filter(|(_, _, l)| serde_json::from_str::<Vec<String>>(l).unwrap_or_default().contains(&tp))
            .map(|(n, t, _)| (n, t)).collect();
        for (n, t) in linking { c.execute("UPDATE notes SET needs_check = 1 WHERE notebook = ?1 AND topic = ?2", params![n, t])?; }
    }
    Ok(gone)
}

pub fn mark_used(c: &Connection, key: &str) -> Result<(), StoreError> {
    if let Some((nb, tp)) = split_key(key) { c.execute("UPDATE notes SET uses = uses + 1 WHERE notebook = ?1 AND topic = ?2", params![nb, tp])?; }
    Ok(())
}

pub fn mark_failed(c: &Connection, key: &str) -> Result<(), StoreError> {
    if let Some((nb, tp)) = split_key(key) { c.execute("UPDATE notes SET failed = 1 WHERE notebook = ?1 AND topic = ?2", params![nb, tp])?; }
    Ok(())
}

pub fn keep_pending(c: &Connection) -> Result<usize, StoreError> {
    Ok(c.execute("UPDATE notes SET pending_job = NULL WHERE pending_job IS NOT NULL", [])?)
}

pub fn discard_pending(c: &Connection) -> Result<usize, StoreError> {
    Ok(c.execute("DELETE FROM notes WHERE pending_job IS NOT NULL", [])?)
}

const COMPUTER_TOP: usize = 15;
const SKILL_TOP: usize = 10;
const MATCHING: usize = 5;
const BLOCK_CAP: usize = 3000;
const STOP: [&str; 16] = ["the", "and", "for", "with", "from", "that", "this", "into", "your", "you", "what", "how", "then", "are", "was", "please"];

fn words_of(t: &str) -> std::collections::HashSet<String> {
    t.to_lowercase().split(|ch: char| !ch.is_alphanumeric()).filter(|w| w.chars().count() >= 3 && !STOP.contains(w)).map(String::from).collect()
}

/// The `top` most-used, then up to `extra` more that share a word with the job.
/// ponytail: word overlap is the search; embeddings if it misses too often.
fn pick(notes: Vec<Note>, top: usize, extra: usize, want: &std::collections::HashSet<String>) -> Vec<Note> {
    let (mut out, rest): (Vec<Note>, Vec<Note>) = (notes.iter().take(top).cloned().collect(), notes.into_iter().skip(top).collect());
    out.extend(rest.into_iter().filter(|n| !words_of(&format!("{} {}", n.topic, n.text)).is_disjoint(want)).take(extra));
    out
}

fn line(n: &Note) -> String {
    let mut s = format!("- {}: {}", key(&n.notebook, &n.topic), n.text);
    if !n.steps.is_empty() { s.push_str(&format!(" (did: {})", n.steps)); }
    if n.failed { s.push_str(" (FAILED last time — fix or remove it)"); }
    if n.needs_check { s.push_str(" (needs checking)"); }
    s
}

/// The tips a job starts with (Phase 3 §3), and the keys of the entries shown.
pub fn for_job(c: &Connection, text: &str, skills: &[String]) -> Result<(String, Vec<String>), StoreError> {
    let want = words_of(text);
    let mut shown: Vec<Note> = pick(list(c, THIS_COMPUTER)?, COMPUTER_TOP, MATCHING, &want);
    for skill in skills.iter().map(|s| norm_notebook(s)).filter(|s| !s.is_empty() && s != THIS_COMPUTER).take(3) {
        let picked = pick(list(c, &skill)?, SKILL_TOP, MATCHING, &want);
        let links: Vec<String> = picked.iter().flat_map(|n| n.links.clone()).collect();
        shown.extend(picked);
        for l in links {
            if shown.iter().any(|n| n.notebook == THIS_COMPUTER && n.topic == l) { continue; }
            if let Some(n) = get(c, THIS_COMPUTER, &l)? { shown.push(n); }
        }
    }
    // Over the cap: the least-used lines go first, the latest of equals before the earlier.
    const HEADER: &str = "What you learned before on this computer (tips, not orders — check them; follow a tip that fits, and a FAILED one needs a different way):\n";
    while !shown.is_empty() && HEADER.chars().count() + shown.iter().map(|n| line(n).chars().count() + 1).sum::<usize>() > BLOCK_CAP {
        let min = shown.iter().map(|n| n.uses).min().unwrap_or(0);
        let i = shown.iter().rposition(|n| n.uses == min).unwrap();
        shown.remove(i);
    }
    if shown.is_empty() { return Ok((String::new(), vec![])); }
    let block = format!("{HEADER}{}", shown.iter().map(line).collect::<Vec<_>>().join("\n"));
    Ok((block, shown.iter().map(|n| key(&n.notebook, &n.topic)).collect()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn db() -> Connection { let c = Connection::open_in_memory().unwrap(); init(&c).unwrap(); c }
    fn note(nb: &str, topic: &str, text: &str) -> Note { Note { notebook: nb.into(), topic: topic.into(), kind: "technique".into(), text: text.into(), ..Default::default() } }

    #[test]
    fn put_replaces_by_topic_and_keeps_the_uses() {
        let c = db();
        put(&c, &note("This Computer", " Open  a Website ", "old way"), None).unwrap();
        mark_used(&c, "this computer/open a website").unwrap();
        mark_failed(&c, "this computer/open a website").unwrap();
        put(&c, &note("this computer", "open a website", "better way"), None).unwrap();
        let l = list(&c, THIS_COMPUTER).unwrap();
        assert_eq!(l.len(), 1, "one line per topic: {l:?}");
        assert_eq!((l[0].text.as_str(), l[0].uses, l[0].failed), ("better way", 1, false));
    }

    #[test]
    fn a_full_notebook_drops_the_least_used_oldest_never_the_new_entry() {
        let c = db();
        for i in 0..MAX_ENTRIES { put(&c, &note("blender", &format!("t{i}"), "x"), None).unwrap(); mark_used(&c, &format!("blender/t{i}")).unwrap(); }
        mark_used(&c, "blender/t0").unwrap();
        put(&c, &note("blender", "brand new", "x"), None).unwrap();
        let l = list(&c, "blender").unwrap();
        assert_eq!(l.len() as i64, MAX_ENTRIES);
        assert!(l.iter().any(|n| n.topic == "brand new"), "the new entry stays");
        assert!(l.iter().any(|n| n.topic == "t0"), "the most-used stays");
        assert!(!l.iter().any(|n| n.topic == "t1"), "the oldest of the least-used went");
    }

    #[test]
    fn removing_a_computer_entry_marks_the_skill_entries_pointing_to_it() {
        let c = db();
        put(&c, &note(THIS_COMPUTER, "start blender", "open_app blender"), None).unwrap();
        let mut sphere = note("blender", "make a round object", "add a uv sphere");
        sphere.links = vec!["Start Blender".into()];
        put(&c, &sphere, None).unwrap();
        put(&c, &note("blender", "unrelated", "x"), None).unwrap();
        assert!(remove(&c, THIS_COMPUTER, "start blender").unwrap());
        let l = list(&c, "blender").unwrap();
        assert!(l.iter().find(|n| n.topic == "make a round object").unwrap().needs_check);
        assert!(!l.iter().find(|n| n.topic == "unrelated").unwrap().needs_check);
    }

    #[test]
    fn pending_entries_are_hidden_until_kept_and_gone_when_discarded() {
        let c = db();
        put(&c, &note(THIS_COMPUTER, "install gimp", "install gimp"), Some("job-1")).unwrap();
        assert!(list(&c, THIS_COMPUTER).unwrap().is_empty());
        assert!(notebooks(&c).unwrap().is_empty());
        assert_eq!(keep_pending(&c).unwrap(), 1);
        assert_eq!(list(&c, THIS_COMPUTER).unwrap().len(), 1);
        put(&c, &note(THIS_COMPUTER, "install inkscape", "install inkscape"), Some("job-2")).unwrap();
        assert_eq!(discard_pending(&c).unwrap(), 1);
        assert_eq!(list(&c, THIS_COMPUTER).unwrap().len(), 1);
    }

    #[test]
    fn this_computer_is_listed_first() {
        let c = db();
        put(&c, &note("blender", "a", "x"), None).unwrap();
        put(&c, &note("aardvark care", "a", "x"), None).unwrap();
        put(&c, &note(THIS_COMPUTER, "a", "x"), None).unwrap();
        assert_eq!(notebooks(&c).unwrap(), vec![THIS_COMPUTER, "aardvark care", "blender"]);
    }

    #[test]
    fn text_and_steps_are_capped() {
        let c = db();
        let mut n = note("x", "t", &"a".repeat(500));
        n.steps = "s".repeat(900);
        put(&c, &n, None).unwrap();
        let got = get(&c, "x", "t").unwrap().unwrap();
        assert_eq!((got.text.len(), got.steps.len()), (200, 400));
    }

    #[test]
    fn for_job_shows_the_most_used_and_the_matching_entries() {
        let c = db();
        for i in 0..20 { put(&c, &note(THIS_COMPUTER, &format!("basic {i}"), "x"), None).unwrap(); }
        for i in 0..15 { for _ in 0..(20 - i) { mark_used(&c, &format!("this computer/basic {i}")).unwrap(); } }
        put(&c, &note(THIS_COMPUTER, "open a website", "open_app firefox with the address"), None).unwrap();
        let (block, shown) = for_job(&c, "go to the weather website", &[]).unwrap();
        assert!(block.starts_with("What you learned before"), "{block}");
        assert!(shown.contains(&"this computer/basic 0".to_string()));
        assert!(shown.contains(&"this computer/open a website".to_string()), "matched by 'website': {shown:?}");
        assert!(!shown.contains(&"this computer/basic 19".to_string()), "never used and not matching");
        assert!(block.contains("- this computer/open a website: open_app firefox with the address"), "{block}");
    }

    #[test]
    fn a_skill_brings_its_linked_computer_entries_and_marks_show() {
        let c = db();
        for i in 0..20 { put(&c, &note(THIS_COMPUTER, &format!("basic {i}"), "x"), None).unwrap(); mark_used(&c, &format!("this computer/basic {i}")).unwrap(); }
        put(&c, &note(THIS_COMPUTER, "start blender", "open_app blender"), None).unwrap();
        let mut sphere = note("blender", "make a round object", "add a uv sphere");
        sphere.links = vec!["start blender".into()];
        sphere.steps = "open_app blender".into();
        put(&c, &sphere, None).unwrap();
        put(&c, &note("blender", "bevel edges", "ctrl+b"), None).unwrap();
        mark_failed(&c, "blender/bevel edges").unwrap();
        let (block, shown) = for_job(&c, "a ball", &["Blender".into()]).unwrap();
        assert!(shown.contains(&"this computer/start blender".to_string()), "pulled in by the link: {shown:?}");
        assert!(block.contains("(did: open_app blender)"), "{block}");
        assert!(block.contains("bevel edges: ctrl+b (FAILED last time"), "{block}");
    }

    #[test]
    fn nothing_learned_means_no_block_and_the_block_is_capped() {
        let c = db();
        assert_eq!(for_job(&c, "anything", &[]).unwrap(), (String::new(), vec![]));
        for i in 0..20 { put(&c, &note(THIS_COMPUTER, &format!("t{i}"), &"w".repeat(200)), None).unwrap(); }
        let (block, _) = for_job(&c, "x", &[]).unwrap();
        assert!(block.chars().count() <= 3000, "{}", block.len());
    }
}
