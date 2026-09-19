# Phase 3 — Skills — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** The AI keeps notebooks of what it learned: "This computer" (read on every job) plus one
per craft (Blender, web design…). It reads the useful entries at the start of each job, updates them
in one learning turn after the job, and the owner sees and deletes them from a Skills button.

**Architecture:** A `notes` table in the engine's SQLite database (`core::notes`, free functions over
a `&Connection`). The tips block is computed once when a job starts and saved on the job, so
`prompt::job_turn` keeps its signature. A new `learn` move is legal only in the learning turn, which
`Engine::finish` runs after Done/Failed. `core::learn::apply` checks the move against the job record
(the steps come from the record, never from the model). The service answers `skills`/`forget`
requests straight from the database, so the Skills screen works while a job runs. Keep/Discard are
fixed words the engine handles without a model call, like `undo`.

**Tech Stack:** Rust (rusqlite 0.32 bundled, serde_json with `preserve_order`), GTK4 for the rail.
No new dependencies. There's no local Rust toolchain: "run the tests" means push the branch and
run `gh workflow run build.yml --ref skills`, then `gh run watch` (the `test` job runs
`cargo test`). Batch pushes: one CI run can cover several tasks, but read every failure before
moving on.

**Spec:** `docs/superpowers/specs/2026-09-19-phase3-skills-design.md`

## Global Constraints

- The "This computer" notebook's name is exactly `this computer` (lower case). Every notebook name
  and topic is normalised: trimmed, lower-cased, inner whitespace collapsed, `/` replaced by a space
  in notebook names.
- An entry key on the wire and in prompts is `notebook/topic` (split at the first `/`).
- `MAX_ENTRIES = 200` per notebook; entry text ≤ 200 chars; entry steps ≤ 400 chars; tips block ≤ 3000 chars.
- Tips selection: This computer 15 most-used + 5 matching; each skill 10 most-used + 5 matching + its linked computer entries. At most 3 skills per job.
- Every new `Job` field gets `#[serde(default)]` (standing convention I3).
- In `MOVE_SCHEMA` the discriminator (`move`) is the first key of every object; new moves are appended at the END of `oneOf` (tests index `oneOf[5]`).
- Fixed words: `keep what you learned`, `discard what you learned` (compared after `engine::normalize`).
- A feature the owner can see edits `runtime/rail/src/guide.txt` in the same commit (memory: guide-stays-current).
- Branch: `skills` from `master`. Commit messages follow the repo's style (`feat(core): …`) and end with `Co-Authored-By: Claude Opus 5 <noreply@anthropic.com>`.

---

### Task 1: The notebook store

**Files:**
- Create: `runtime/core/src/notes.rs`
- Modify: `runtime/core/src/lib.rs` (add `pub mod notes;`)
- Modify: `runtime/core/src/store.rs` (`init` calls `notes::init` and sets a busy timeout; add `pub fn conn(&self) -> &Connection`)

**Interfaces:**
- Produces:
  - `pub const THIS_COMPUTER: &str = "this computer";`, `pub const MAX_ENTRIES: i64 = 200;`
  - `pub struct Note { pub notebook: String, pub topic: String, pub kind: String, pub text: String, pub steps: String, pub links: Vec<String>, pub uses: i64, pub failed: bool, pub needs_check: bool }` (Debug, Clone, PartialEq, Default)
  - `pub fn norm(s: &str) -> String`, `pub fn norm_notebook(s: &str) -> String`, `pub fn key(notebook: &str, topic: &str) -> String`
  - `pub fn init(c: &Connection) -> Result<(), StoreError>`
  - `pub fn put(c: &Connection, n: &Note, pending_job: Option<&str>) -> Result<(), StoreError>`
  - `pub fn get(c: &Connection, notebook: &str, topic: &str) -> Result<Option<Note>, StoreError>`
  - `pub fn list(c: &Connection, notebook: &str) -> Result<Vec<Note>, StoreError>` (kept entries only, most-used first, then newest)
  - `pub fn notebooks(c: &Connection) -> Result<Vec<String>, StoreError>` (`this computer` first, the rest alphabetical)
  - `pub fn all(c: &Connection) -> Result<Vec<(String, Vec<Note>)>, StoreError>`
  - `pub fn remove(c: &Connection, notebook: &str, topic: &str) -> Result<bool, StoreError>`
  - `pub fn mark_used(c: &Connection, key: &str) -> Result<(), StoreError>`, `pub fn mark_failed(c: &Connection, key: &str) -> Result<(), StoreError>`
  - `pub fn keep_pending(c: &Connection) -> Result<usize, StoreError>`, `pub fn discard_pending(c: &Connection) -> Result<usize, StoreError>`

- [ ] **Step 1: Branch**

```bash
git checkout -b skills master
```

- [ ] **Step 2: Write the module with its failing tests**

Create `runtime/core/src/notes.rs`:

```rust
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
}
```

Add `pub mod notes;` to `runtime/core/src/lib.rs` after `pub mod moves;`.

In `runtime/core/src/store.rs`, at the end of `init` before `Ok(Self { conn })`:

```rust
        // The service reads and deletes notebook entries on its own connection while the engine
        // works (Phase 3 §6): wait for the other side's write rather than fail on a locked file.
        conn.busy_timeout(std::time::Duration::from_secs(5))?;
        crate::notes::init(&conn)?;
```

and in `impl Store`:

```rust
    /// The notebooks live in this database too (`crate::notes` works on the connection).
    pub fn conn(&self) -> &Connection { &self.conn }
```

- [ ] **Step 3: Push and run the tests**

```bash
git add runtime/core/src/notes.rs runtime/core/src/lib.rs runtime/core/src/store.rs
git commit -m "feat(core): the notebooks — one line per topic, a cap that keeps the used ones, links marked when their target goes"
git push -u origin skills
gh workflow run build.yml --ref skills && sleep 5 && gh run watch $(gh run list --branch skills --limit 1 --json databaseId -q '.[0].databaseId')
```

Expected: the `test` job passes, including the six `notes::tests`. The commit was made before the
run on purpose (CI builds commits). If anything fails, fix it and add a `fix(core): …` commit.

---

### Task 2: What a job reads — `notes::for_job`

**Files:**
- Modify: `runtime/core/src/notes.rs` (add `for_job` and its helpers and tests)

**Interfaces:**
- Consumes: Task 1's `list`, `get`, `key`, `norm_notebook`, `THIS_COMPUTER`.
- Produces: `pub fn for_job(c: &Connection, text: &str, skills: &[String]) -> Result<(String, Vec<String>), StoreError>`. It returns the tips block (empty string when there is nothing) and the keys shown (`notebook/topic`).

- [ ] **Step 1: Write the failing tests** (append inside `mod tests`)

```rust
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
```

- [ ] **Step 2: Implement** (above `#[cfg(test)]`)

```rust
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
```

- [ ] **Step 3: Commit, push, run the tests**

```bash
git add runtime/core/src/notes.rs
git commit -m "feat(core): the tips a job reads — the most-used, the matching, the linked computer entries, 3000 chars at most"
git push && gh workflow run build.yml --ref skills
```

Expected: the three new tests pass (the CI run can be shared with Task 3).

---

### Task 3: The `learn` move, `skills` on `start`, and new job fields

**Files:**
- Modify: `runtime/core/src/moves.rs` (add `LearnEntry`, `Move::Learn`, `skills` on `Move::Start`, tests)
- Modify: `runtime/core/src/schema.rs` (`learn` in `MOVE_SCHEMA`, `skills` in `start`)
- Modify: `runtime/core/src/model.rs` (`FakeModel` honours a learn-only prompt)
- Modify: `runtime/core/src/job.rs` (`skills`, `notes_block`, `shown_notes`)
- Modify: every `Move::Start { … }` literal: `runtime/core/src/engine.rs` (tests) and `runtime/core/tests/service.rs`. Find them with `grep -rn "Move::Start {" runtime`.

**Interfaces:**
- Produces:
  - `pub struct LearnEntry { pub notebook: String, pub topic: String, pub kind: String, pub text: String, #[serde(default)] pub steps: Vec<usize>, #[serde(default)] pub links: Vec<String> }`
  - `Move::Learn { entries: Vec<LearnEntry>, #[serde(default)] used: Vec<String>, #[serde(default)] wrong: Vec<String>, #[serde(default)] remove: Vec<String> }`
  - `Move::Start { …, #[serde(default)] skills: Vec<String>, remember }` (the field goes after `understood`)
  - `Job { …, pub skills: Vec<String>, pub notes_block: String, pub shown_notes: Vec<String> }`, all `#[serde(default)]`

- [ ] **Step 1: Failing tests**

In `moves.rs` `parses_every_move`, add to `cases`:

```rust
            r#"{"move":"start","project":"ball","new_project":true,"description":"d","goal":"g","creative":false,"understood":"u","skills":["blender"]}"#,
            r#"{"move":"learn","entries":[{"notebook":"this computer","topic":"open a website","kind":"technique","text":"open_app firefox with the address","steps":[7],"links":[]}],"used":["this computer/open a website"],"wrong":[],"remove":[]}"#,
            r#"{"move":"learn","entries":[]}"#,
```

and change `move_names_match_schema` to expect
`["reply", "start", "housekeep", "ask", "plan", "act", "replan", "done", "give_up", "learn"]`.

In `schema.rs` tests add:

```rust
    #[test]
    fn start_offers_skills_and_learn_entries_have_the_three_kinds() {
        let v = value();
        assert_eq!(v["oneOf"][1]["properties"]["skills"]["maxItems"], 3);
        let learn = &v["oneOf"][9];
        assert_eq!(learn["properties"]["move"]["enum"][0], "learn");
        assert_eq!(learn["properties"]["entries"]["items"]["properties"]["kind"]["enum"], serde_json::json!(["technique", "pitfall", "taste"]));
    }
```

In `job.rs` tests add:

```rust
    #[test]
    fn a_job_saved_before_skills_still_deserialises() {
        let job = Job::new("p", "/data/projects/p", "g", true, "u");
        let mut value = serde_json::to_value(&job).unwrap();
        let obj = value.as_object_mut().unwrap();
        for k in ["skills", "notes_block", "shown_notes"] { assert!(obj.remove(k).is_some(), "{k}"); }
        let back: Job = serde_json::from_value(value).unwrap();
        assert!(back.skills.is_empty() && back.notes_block.is_empty() && back.shown_notes.is_empty());
    }
```

In `model.rs` tests add:

```rust
    #[test]
    fn the_fake_never_spends_a_scripted_move_on_a_learning_turn_that_is_not_learn() {
        let m = FakeModel::new(vec![Move::Reply { text: "next job's move".into(), remember: None }]);
        let learn_only = Prompt { system: String::new(), user: String::new(), allowed: vec!["learn"], image: None };
        assert!(m.next_move(&learn_only).is_err());
        assert!(matches!(m.next_move(&learn_only.clone()), Err(_)));
        let any = Prompt { allowed: vec![], ..learn_only };
        assert!(matches!(m.next_move(&any), Ok(Move::Reply { .. })), "still there for the next prompt");
    }
```

- [ ] **Step 2: Implement**

`moves.rs`: add above `pub enum Move`:

```rust
/// One notebook entry the learning turn proposes (Phase 3 §4). `steps` are job step numbers;
/// what is stored comes from those steps in the record, never from these words.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LearnEntry {
    pub notebook: String,
    pub topic: String,
    pub kind: String,
    pub text: String,
    #[serde(default)]
    pub steps: Vec<usize>,
    #[serde(default)]
    pub links: Vec<String>,
}
```

In `Move::Start`, after `understood: String,`:

```rust
        /// The craft notebooks this job belongs to (Phase 3 §2); none for a plain errand.
        #[serde(default)]
        skills: Vec<String>,
```

At the end of the enum, after `GiveUp`:

```rust
    /// Legal only in the learning turn after a job (Phase 3 §4).
    Learn {
        entries: Vec<LearnEntry>,
        #[serde(default)] used: Vec<String>,
        #[serde(default)] wrong: Vec<String>,
        #[serde(default)] remove: Vec<String>,
    },
```

`schema.rs`: in the `start` object, insert `"skills": {"type":"array","items":{"type":"string"},"maxItems":3},` after `"understood": {"type":"string"},`, and add `"skills"` to its `required` after `"understood"`. Append as the last `oneOf` item (after `give_up`, with a comma):

```json
    { "type":"object", "properties": { "move": {"enum":["learn"]}, "entries": {"type":"array","maxItems":5,"items":{"type":"object","properties":{"notebook":{"type":"string"},"topic":{"type":"string"},"kind":{"enum":["technique","pitfall","taste"]},"text":{"type":"string"},"steps":{"type":"array","items":{"type":"integer","minimum":1},"maxItems":10},"links":{"type":"array","items":{"type":"string"},"maxItems":5}},"required":["notebook","topic","kind","text","steps","links"],"additionalProperties":false}}, "used": {"type":"array","items":{"type":"string"}}, "wrong": {"type":"array","items":{"type":"string"}}, "remove": {"type":"array","items":{"type":"string"}} }, "required":["move","entries","used","wrong","remove"], "additionalProperties": false }
```

`model.rs`, `impl Model for FakeModel`:

```rust
    fn next_move(&self, prompt: &Prompt) -> Result<Move, ModelError> {
        self.prompts.borrow_mut().push(prompt.clone());
        // The real grammar forces a learn in the learning turn. A script that has none there
        // answers "exhausted" and keeps its next move for the next job's prompt.
        if prompt.allowed == ["learn"] && !matches!(self.queue.borrow().front(), Some(Move::Learn { .. })) {
            return Err(ModelError::Exhausted);
        }
        self.queue.borrow_mut().pop_front().ok_or(ModelError::Exhausted)
    }
```

`job.rs`: add to `Job` after `done_gated`:

```rust
    /// The craft notebooks this job belongs to (Phase 3 §2). `serde(default)`: I3.
    #[serde(default)]
    pub skills: Vec<String>,
    /// The tips shown on every turn, fixed when the job starts (Phase 3 §3). `serde(default)`: I3.
    #[serde(default)]
    pub notes_block: String,
    /// The keys (`notebook/topic`) of those tips, so the learning turn can mark only what was shown.
    #[serde(default)]
    pub shown_notes: Vec<String>,
```

and in `Job::new` add `skills: vec![], notes_block: String::new(), shown_notes: vec![],`.

Add `skills: vec![],` to every `Move::Start { … }` literal (`grep -rn "Move::Start {" runtime`). The engine's destructuring in `handle_inner` becomes
`Move::Start { project, new_project: _, description, goal, creative, understood, skills: _, remember }`
for now (Task 5 uses it).

- [ ] **Step 3: Check the allowed-move filter knows `learn`**

Read `narrow_schema` in `runtime/core/src/model.rs` (line ~128). It filters `oneOf` by the names in
`allowed`, so `["learn"]` leaves only the learn object. If it has a hard-coded list of names, add
`learn` to it.

- [ ] **Step 4: Commit, push, run the tests**

```bash
git add runtime/core
git commit -m "feat(core): the learn move, skills on start, the job keeps its tips"
git push && gh workflow run build.yml --ref skills && sleep 5 && gh run watch $(gh run list --branch skills --limit 1 --json databaseId -q '.[0].databaseId')
```

Expected: all tests pass (Tasks 2 and 3).

---

### Task 4: Applying a learning turn — `core::learn`

**Files:**
- Create: `runtime/core/src/learn.rs`
- Modify: `runtime/core/src/lib.rs` (`pub mod learn;`)
- Modify: `runtime/core/src/prompt.rs` (`compact_action` becomes `pub(crate)`)

**Interfaces:**
- Consumes: `notes::{put, get, remove, mark_used, mark_failed, discard_pending, key, norm, norm_notebook, Note, THIS_COMPUTER}`, `moves::LearnEntry`, `job::Job`, `prompt::compact_action`.
- Produces:
  - `pub struct Learned { pub lines: Vec<String>, pub pending: bool }`
  - `pub fn apply(c: &Connection, job: &Job, entries: Vec<LearnEntry>, used: &[String], wrong: &[String], remove: &[String], passed: bool) -> Result<Learned, StoreError>`

- [ ] **Step 1: Write the module with failing tests**

```rust
//! The learning turn, checked against the job record (Phase 3 §4–§5). The model proposes; the
//! record decides: a technique's steps are the steps that ran, never the model's account of them.
use crate::job::{Job, StepRecord};
use crate::moves::LearnEntry;
use crate::notes::{self, Note, THIS_COMPUTER};
use crate::store::StoreError;
use executor::action::Action;
use rusqlite::Connection;
use std::path::Path;

#[derive(Debug, Default, PartialEq)]
pub struct Learned { pub lines: Vec<String>, pub pending: bool }

/// Whether a step changed the machine outside the job's own folder (Phase 3 §5).
fn changes_machine(a: &Action, folder: &str) -> bool {
    let outside = |p: &str| Path::new(p).is_absolute() && !Path::new(p).starts_with(folder);
    match a {
        Action::Install { .. } | Action::Remove { .. } | Action::Service { .. } | Action::SetSetting { .. } => true,
        Action::WriteFile { path, .. } | Action::EditFile { path, .. } | Action::MakeDir { path } => outside(path),
        _ => false,
    }
}

fn step(job: &Job, n: usize) -> Option<&StepRecord> { n.checked_sub(1).and_then(|i| job.steps.get(i)) }

fn steps_text(job: &Job, ns: &[usize]) -> String {
    ns.iter().filter_map(|&n| step(job, n)).map(|s| crate::prompt::compact_action(&s.action).chars().take(150).collect::<String>()).collect::<Vec<_>>().join("; ")
}

pub fn apply(c: &Connection, job: &Job, entries: Vec<LearnEntry>, used: &[String], wrong: &[String], remove: &[String], passed: bool) -> Result<Learned, StoreError> {
    let mut out = Learned::default();
    // What an earlier job left waiting for Keep/Discard and nobody answered goes now (spec §5).
    notes::discard_pending(c)?;
    let shown = |k: &String| { let (nb, tp) = k.split_once('/').unwrap_or(("", k)); job.shown_notes.contains(&notes::key(nb, tp)) };
    for k in used.iter().filter(|k| shown(k)) { notes::mark_used(c, k)?; }
    for k in wrong.iter().filter(|k| shown(k)) {
        notes::mark_failed(c, k)?;
        out.lines.push(format!("Marked as not working: {k}"));
    }
    // A job that did not pass proved nothing new.
    if !passed { return Ok(out); }
    for k in remove.iter().filter(|k| shown(k)) {
        let (nb, tp) = k.split_once('/').unwrap_or(("", k));
        if notes::remove(c, nb, tp)? { out.lines.push(format!("Forgot: {k}")); }
    }
    let heard_the_user = !job.request.is_empty() || !job.answers.is_empty();
    let computer_topics: Vec<String> = entries.iter().filter(|e| notes::norm_notebook(&e.notebook) == THIS_COMPUTER).map(|e| notes::norm(&e.topic)).collect();
    for e in entries {
        let (nb, tp) = (notes::norm_notebook(&e.notebook), notes::norm(&e.topic));
        if nb.is_empty() || tp.is_empty() || e.text.trim().is_empty() { continue; }
        let cited: Vec<&StepRecord> = e.steps.iter().filter_map(|&n| step(job, n)).collect();
        if cited.len() != e.steps.len() { continue; } // a step that does not exist
        let proof: Vec<usize> = match e.kind.as_str() {
            "technique" if !cited.is_empty() && cited.iter().all(|s| s.ok) => e.steps.clone(),
            "pitfall" => {
                let first_fail = e.steps.iter().copied().filter(|&n| !step(job, n).unwrap().ok).min();
                let fixes: Vec<usize> = e.steps.iter().copied().filter(|&n| step(job, n).unwrap().ok && Some(n) > first_fail).collect();
                if first_fail.is_none() || fixes.is_empty() { continue; }
                fixes
            }
            "taste" if e.steps.is_empty() && heard_the_user => vec![],
            _ => continue,
        };
        let links = if nb == THIS_COMPUTER { vec![] } else {
            let mut keep = vec![];
            for l in e.links.iter().map(|l| notes::norm(l)) {
                if computer_topics.contains(&l) || notes::get(c, THIS_COMPUTER, &l)?.is_some() { keep.push(l); }
            }
            keep
        };
        let pending = proof.iter().filter_map(|&n| step(job, n)).any(|s| changes_machine(&s.action, &job.folder));
        let note = Note { notebook: nb.clone(), topic: tp.clone(), kind: e.kind.clone(), text: e.text.trim().to_string(), steps: steps_text(job, &proof), links, ..Default::default() };
        notes::put(c, &note, pending.then_some(job.id.as_str()))?;
        out.pending |= pending;
        out.lines.push(if pending { format!("Learned, if you keep it: {tp} ({nb})") } else { format!("Learned: {tp} ({nb})") });
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::notes::{init, list};

    fn db() -> Connection { let c = Connection::open_in_memory().unwrap(); init(&c).unwrap(); c }
    fn rec(a: Action, ok: bool) -> StepRecord { StepRecord { plan_step: 1, action: a, ok, detail: String::new() } }
    fn open(url: &str) -> Action { Action::OpenApp { name: format!("firefox {url}"), visible: true } }
    fn job(steps: Vec<StepRecord>) -> Job {
        let mut j = Job::new_housekeeping("/data/housekeeping", "go to a site", "u");
        j.request = "open the browser and go to example.org".into();
        j.steps = steps;
        j
    }
    fn entry(nb: &str, topic: &str, kind: &str, steps: Vec<usize>) -> LearnEntry {
        LearnEntry { notebook: nb.into(), topic: topic.into(), kind: kind.into(), text: "the way".into(), steps, links: vec![] }
    }

    #[test]
    fn a_technique_stores_the_steps_from_the_record_not_the_model() {
        let c = db();
        let j = job(vec![rec(Action::ScreenLook { cell: None }, true), rec(open("example.org"), true)]);
        let l = apply(&c, &j, vec![entry("This computer", "Open a website", "technique", vec![2])], &[], &[], &[], true).unwrap();
        assert_eq!(l.lines, vec!["Learned: open a website (this computer)".to_string()]);
        let n = &list(&c, THIS_COMPUTER).unwrap()[0];
        assert!(n.steps.contains("open_app") && n.steps.contains("example.org"), "{}", n.steps);
    }

    #[test]
    fn a_technique_citing_a_failed_or_missing_step_is_dropped() {
        let c = db();
        let j = job(vec![rec(open("a"), false), rec(open("b"), true)]);
        let l = apply(&c, &j, vec![entry("this computer", "x", "technique", vec![1]), entry("this computer", "y", "technique", vec![9]), entry("this computer", "z", "technique", vec![])], &[], &[], &[], true).unwrap();
        assert!(l.lines.is_empty(), "{:?}", l.lines);
        assert!(list(&c, THIS_COMPUTER).unwrap().is_empty());
    }

    #[test]
    fn a_pitfall_needs_a_failure_and_a_later_fix() {
        let c = db();
        let j = job(vec![rec(open("a"), false), rec(open("b"), true)]);
        apply(&c, &j, vec![entry("this computer", "fix", "pitfall", vec![1, 2]), entry("this computer", "nofix", "pitfall", vec![2])], &[], &[], &[], true).unwrap();
        let l = list(&c, THIS_COMPUTER).unwrap();
        assert_eq!(l.len(), 1);
        assert!(l[0].steps.contains("b") && !l[0].steps.contains("firefox a"), "only the fix is kept as proof: {}", l[0].steps);
    }

    #[test]
    fn taste_needs_the_users_words_and_no_steps() {
        let c = db();
        let mut j = job(vec![]);
        apply(&c, &j, vec![entry("web design", "colours", "taste", vec![])], &[], &[], &[], true).unwrap();
        assert_eq!(list(&c, "web design").unwrap().len(), 1);
        j.request.clear();
        apply(&c, &j, vec![entry("web design", "fonts", "taste", vec![])], &[], &[], &[], true).unwrap();
        assert_eq!(list(&c, "web design").unwrap().len(), 1, "no words from the user, no taste");
    }

    #[test]
    fn used_wrong_and_remove_touch_only_what_was_shown() {
        let c = db();
        for t in ["shown", "hidden"] { notes::put(&c, &Note { notebook: THIS_COMPUTER.into(), topic: t.into(), kind: "technique".into(), text: "x".into(), ..Default::default() }, None).unwrap(); }
        let mut j = job(vec![]);
        j.shown_notes = vec!["this computer/shown".into()];
        apply(&c, &j, vec![], &["this computer/shown".into(), "this computer/hidden".into()], &["this computer/hidden".into()], &[], true).unwrap();
        let l = list(&c, THIS_COMPUTER).unwrap();
        let get = |t: &str| l.iter().find(|n| n.topic == t).unwrap().clone();
        assert_eq!((get("shown").uses, get("hidden").uses, get("hidden").failed), (1, 0, false));
        apply(&c, &j, vec![], &[], &[], &["this computer/hidden".into(), "this computer/shown".into()], true).unwrap();
        assert_eq!(list(&c, THIS_COMPUTER).unwrap().iter().map(|n| n.topic.clone()).collect::<Vec<_>>(), vec!["hidden"]);
    }

    #[test]
    fn a_failed_job_only_marks_and_counts() {
        let c = db();
        notes::put(&c, &Note { notebook: THIS_COMPUTER.into(), topic: "t".into(), kind: "technique".into(), text: "x".into(), ..Default::default() }, None).unwrap();
        let mut j = job(vec![rec(open("a"), true)]);
        j.shown_notes = vec!["this computer/t".into()];
        let l = apply(&c, &j, vec![entry("this computer", "new", "technique", vec![1])], &[], &["this computer/t".into()], &["this computer/t".into()], false).unwrap();
        assert_eq!(l.lines, vec!["Marked as not working: this computer/t".to_string()]);
        let all = list(&c, THIS_COMPUTER).unwrap();
        assert_eq!(all.len(), 1, "nothing new, nothing removed");
        assert!(all[0].failed);
    }

    #[test]
    fn a_step_that_changes_the_machine_waits_for_keep() {
        let c = db();
        let j = job(vec![rec(Action::Install { packages: vec!["gimp".into()] }, true), rec(Action::WriteFile { path: "/data/housekeeping/x".into(), contents: "y".into() }, true)]);
        let l = apply(&c, &j, vec![entry("this computer", "get gimp", "technique", vec![1]), entry("this computer", "scratch note", "technique", vec![2])], &[], &[], &[], true).unwrap();
        assert!(l.pending);
        assert_eq!(l.lines, vec!["Learned, if you keep it: get gimp (this computer)".to_string(), "Learned: scratch note (this computer)".to_string()]);
        assert_eq!(list(&c, THIS_COMPUTER).unwrap().len(), 1, "the install waits unseen");
    }

    #[test]
    fn a_skill_entry_keeps_only_links_that_exist() {
        let c = db();
        let j = job(vec![rec(open("blender"), true)]);
        let mut e = entry("blender", "make a round object", "technique", vec![1]);
        e.links = vec!["start blender".into(), "nothing like this".into()];
        apply(&c, &j, vec![entry("this computer", "start blender", "technique", vec![1]), e], &[], &[], &[], true).unwrap();
        assert_eq!(list(&c, "blender").unwrap()[0].links, vec!["start blender".to_string()]);
    }
}
```

Add `pub mod learn;` to `lib.rs` (after `pub mod job;`). In `prompt.rs` change `fn compact_action` to `pub(crate) fn compact_action`.

- [ ] **Step 2: Commit, push, run the tests**

```bash
git add runtime/core
git commit -m "feat(core): the learning turn checked against the record — proof from the steps that ran, machine changes wait for keep"
git push && gh workflow run build.yml --ref skills && sleep 5 && gh run watch $(gh run list --branch skills --limit 1 --json databaseId -q '.[0].databaseId')
```

Expected: the eight `learn::tests` pass.

---

### Task 5: Prompts — notebooks at the door, tips in each turn, the learning turn

**Files:**
- Modify: `runtime/core/src/prompt.rs`

**Interfaces:**
- Consumes: `Job::{notes_block, shown_notes}`, `compact_action`.
- Produces:
  - `front_door(instructions: &[String], projects: &[ProjectRow], notebooks: &[String], recent: &[(String, String)], message: &str) -> Prompt`. The new third parameter is the notebook names.
  - `job_turn` unchanged in signature; it prints `job.notes_block` when non-empty.
  - `pub fn learning_turn(job: &Job, passed: bool) -> Prompt` with `allowed: vec!["learn"]`.

- [ ] **Step 1: Failing tests** (in `prompt.rs` tests; also add `&[]` as the third argument at every existing `front_door(` call in the tests and in `engine.rs`)

```rust
    #[test]
    fn the_front_door_lists_the_notebooks_and_asks_for_skills() {
        let p = front_door(&[], &[], &["this computer".into(), "blender".into()], &[], "make a ball in blender");
        assert!(p.user.contains("Skill notebooks: this computer, blender"), "{}", p.user);
        assert!(p.user.contains("skills (0-3"), "{}", p.user);
        assert!(front_door(&[], &[], &[], &[], "hi").user.contains("Skill notebooks: (none yet)"));
    }

    #[test]
    fn the_job_turn_carries_the_tips_fixed_at_start() {
        let mut j = Job::new_housekeeping("/data/housekeeping", "go to a site", "u");
        assert!(!job_turn(&[], &j, None, None).user.contains("What you learned before"));
        j.notes_block = "What you learned before on this computer (tips…):\n- this computer/open a website: open_app firefox".into();
        let user = job_turn(&[], &j, None, None).user;
        assert!(user.contains("- this computer/open a website"), "{user}");
        assert!(user.find("What you learned before").unwrap() < user.find("Steps so far").unwrap(), "{user}");
    }

    #[test]
    fn the_learning_turn_numbers_every_step_and_asks_only_for_learn() {
        let mut j = Job::new_housekeeping("/data/housekeeping", "go to a site", "u");
        j.request = "open example.org".into();
        j.steps = vec![
            StepRecord { plan_step: 1, action: Action::ScreenLook { cell: None }, ok: true, detail: "d".into() },
            StepRecord { plan_step: 1, action: Action::OpenApp { name: "firefox".into(), visible: true }, ok: false, detail: "d".into() },
        ];
        let p = learning_turn(&j, true);
        assert_eq!(p.allowed, vec!["learn"]);
        assert!(p.user.contains("1: ") && p.user.contains("-> ok") && p.user.contains("2: ") && p.user.contains("-> failed"), "{}", p.user);
        assert!(p.user.contains("open example.org"));
        assert!(p.user.contains("\"this computer\""), "{}", p.user);
        let failed = learning_turn(&j, false);
        assert!(failed.user.contains("did not finish") && !failed.user.contains("Write at most 5 entries"), "{}", failed.user);
    }
```

- [ ] **Step 2: Implement**

`front_door`: add the `notebooks: &[String]` parameter after `projects`, then build
`let notebooks_txt = if notebooks.is_empty() { "(none yet)".to_string() } else { notebooks.join(", ") };`
Put `\n\nSkill notebooks: {notebooks_txt}` right after the Projects list in the format string. In
the `start` clause's field list, change `give project, new_project, description, goal, creative, understood` to
`give project, new_project, description, goal, creative, understood, and skills (0-3 craft areas the job belongs to — coding, web design, a program like blender — named from the notebooks listed or a new short name; [] for a plain errand)`.

`job_turn`: build `let tips = if job.notes_block.is_empty() { String::new() } else { format!("\n\n{}", job.notes_block) };`
and insert `{}` for it after `{}{}` (bp_block, last) and before `\n\nSteps so far`, passing `tips`.

`learning_turn` (after `approval_question`):

```rust
/// Every step of the job, numbered as the learning turn cites them: the last 80 in full, compact.
fn learn_steps(job: &Job) -> String {
    let skip = job.steps.len().saturating_sub(80);
    let mut out = if skip > 0 { format!("(steps 1-{skip} not shown)\n") } else { String::new() };
    for (i, s) in job.steps.iter().enumerate().skip(skip) {
        let a: String = compact_action(&s.action).chars().take(150).collect();
        out.push_str(&format!("{}: {a} -> {}\n", i + 1, if s.ok { "ok" } else { "failed" }));
    }
    if out.is_empty() { "(no steps)".into() } else { out }
}

/// The one extra turn after a job (Phase 3 §4): what to keep, which tips helped, which were wrong.
pub fn learning_turn(job: &Job, passed: bool) -> Prompt {
    let tips = if job.notes_block.is_empty() { "(no tips were shown)".to_string() } else { job.notes_block.clone() };
    let ask = if passed {
        "What did you learn that you would want to know straight away next time? Write at most 5 entries; nothing new or nothing hard means entries []. \
         notebook: \"this computer\" for how this computer and its programs are used (opening a program, finding things, where things are); a craft name (python coding, web design, blender…) for doing that craft better. \
         topic: a short general name for what it does (\"open a website\", never the site's own name). \
         kind: technique (steps = the ok steps that did it), pitfall (steps = the failed step and the ok step that fixed it), taste (what the user liked or rejected; steps []). \
         text: one general line, naming in words what changes between uses (\"the address\"). links: \"this computer\" topics a craft entry depends on."
    } else {
        "The job did not finish, so nothing new is learned: entries []."
    };
    let user = format!(
        "The job {}: {}\nGoal: {}\nThe user asked (verbatim): {}\n\nSteps (number: action -> result):\n{}\n{}\n\n{ask}\n\
         used: the tips above you followed (as notebook/topic). wrong: tips above that did not work. remove: tips above that are useless (only after a job that finished).",
        if passed { "passed its check" } else { "did not finish" },
        if job.housekeeping { "housekeeping" } else { &job.project }, job.goal, job.request, learn_steps(job), tips,
    );
    Prompt { system: SYSTEM.into(), user, allowed: vec!["learn"], image: None }
}
```

If `prompt::tests::every_allowed_list_matches_a_real_move` enumerates the prompt builders, add `learning_turn(&job, true).allowed` to it.

- [ ] **Step 2b: Keep the engine compiling**

In `engine.rs` `handle_inner`, the front-door call becomes:

```rust
        let notebooks = crate::notes::notebooks(self.store.conn())?;
        let p = prompt::front_door(&self.store.instructions()?, &self.store.list_projects()?, &notebooks, &self.store.recent_messages(4)?, text);
```

(`EngineError` already converts from `StoreError`; check the `#[from]` list at `engine.rs:24`.)

- [ ] **Step 3: Commit, push, run the tests**

```bash
git add runtime/core
git commit -m "feat(prompt): the notebooks at the door, the tips in every turn, the learning turn"
git push && gh workflow run build.yml --ref skills
```

---

### Task 6: The engine — tips at start, learning after the job, keep/discard

**Files:**
- Modify: `runtime/core/src/engine.rs`
- Modify: `runtime/proto/src/lib.rs` (`Event::Learned`, `job_id()`)
- Modify: `runtime/core/src/event.rs` (`lines` for `Learned`)

**Interfaces:**
- Consumes: `notes::for_job`, `notes::norm_notebook`, `notes::{keep_pending, discard_pending}`, `learn::apply`, `prompt::learning_turn`.
- Produces:
  - `Event::Learned { job_id: String, lines: Vec<String>, #[serde(default)] pending: bool }`
  - `pub fn is_keep_learned(text: &str) -> bool`, `pub fn is_discard_learned(text: &str) -> bool` in `engine.rs`

- [ ] **Step 1: Failing tests** (engine tests module)

```rust
    fn learn_open_site() -> Move {
        Move::Learn { entries: vec![crate::moves::LearnEntry { notebook: "this computer".into(), topic: "open a website".into(), kind: "technique".into(), text: "open_app firefox with the address".into(), steps: vec![1], links: vec![] }], used: vec![], wrong: vec![], remove: vec![] }
    }
    fn hk(goal: &str) -> Move { Move::Housekeep { goal: goal.into(), understood: "Opening it".into(), remember: None } }
    fn open_ff() -> Action { Action::OpenApp { name: "firefox".into(), visible: true } }

    #[test]
    fn a_passed_job_learns_and_the_next_job_starts_with_the_tip() {
        let (mut e, _, _) = engine_with(vec![
            hk("open example.org"), plan(), act(1, open_ff()), done(Action::ScreenLook { cell: None }), learn_open_site(),
            hk("open the weather website"),
        ], "learn-next");
        let ev = crate::testing::events_of(&mut e, "open the browser and go to example.org");
        let done_at = ev.iter().position(|x| matches!(x, Event::Done { .. })).expect("done");
        let learned_at = ev.iter().position(|x| matches!(x, Event::Learned { .. })).expect("learned");
        assert!(done_at < learned_at, "the Done card never waits for the learning");
        let Event::Learned { lines, pending, .. } = &ev[learned_at] else { unreachable!() };
        assert_eq!((lines.clone(), *pending), (vec!["Learned: open a website (this computer)".to_string()], false));
        let _ = e.handle("open the weather website");
        let last = e.model.prompts.borrow().last().unwrap().user.clone();
        assert!(last.contains("- this computer/open a website: open_app firefox with the address"), "{last}");
    }

    #[test]
    fn a_learning_turn_the_model_cannot_answer_never_hurts_the_job() {
        let (mut e, _, _) = engine_with(vec![hk("open it"), plan(), act(1, open_ff()), done(Action::ScreenLook { cell: None })], "learn-none");
        let ev = crate::testing::events_of(&mut e, "open it");
        assert!(ev.iter().any(|x| matches!(x, Event::Done { .. })));
        assert!(!ev.iter().any(|x| matches!(x, Event::Learned { .. })));
        assert_eq!(e.model.prompts.borrow().last().unwrap().allowed, vec!["learn"], "the learning turn was asked");
    }

    #[test]
    fn a_stopped_job_has_no_learning_turn() {
        let (mut e, _, _) = engine_with(vec![start("p", false), Move::Ask { questions: vec!["?".into()], options: vec![] }], "learn-stop");
        e.handle("make p").unwrap();
        e.handle("stop").unwrap();
        assert!(e.model.prompts.borrow().iter().all(|p| p.allowed != vec!["learn"]));
    }

    #[test]
    fn keep_and_discard_are_fixed_words_not_model_calls() {
        let install = Action::Install { packages: vec!["gimp".into()] };
        let learn_install = Move::Learn { entries: vec![crate::moves::LearnEntry { notebook: "this computer".into(), topic: "get gimp".into(), kind: "technique".into(), text: "install gimp".into(), steps: vec![1], links: vec![] }], used: vec![], wrong: vec![], remove: vec![] };
        let (mut e, _, _) = engine_with(vec![hk("get gimp"), plan(), act(1, install), done(run("true")), learn_install], "learn-keep");
        let ev = crate::testing::events_of(&mut e, "install gimp");
        assert!(ev.iter().any(|x| matches!(x, Event::Learned { pending: true, .. })), "{ev:?}");
        assert!(crate::notes::list(e.store.conn(), "this computer").unwrap().is_empty());
        let before = e.model.prompts.borrow().len();
        assert!(e.handle("Keep what you learned.").unwrap()[0].contains("Kept"));
        assert_eq!(e.model.prompts.borrow().len(), before);
        assert_eq!(crate::notes::list(e.store.conn(), "this computer").unwrap().len(), 1);
        assert!(e.handle("discard what you learned").unwrap()[0].contains("nothing waiting"));
    }

    #[test]
    fn start_records_its_skills_and_the_skill_tips() {
        let ball = Move::Start { project: "ball".into(), new_project: true, description: "d".into(), goal: "a ball".into(), creative: true, understood: "Making a ball".into(), skills: vec!["Blender".into(), "this computer".into()], remember: None };
        let (mut e, _, _) = engine_with(vec![ball, Move::Ask { questions: vec!["?".into()], options: vec![] }], "learn-skills");
        crate::notes::put(e.store.conn(), &crate::notes::Note { notebook: "blender".into(), topic: "make a round object".into(), kind: "technique".into(), text: "add a uv sphere".into(), ..Default::default() }, None).unwrap();
        e.handle("make a ball in blender").unwrap();
        let job = e.open_job().unwrap().unwrap();
        assert_eq!(job.skills, vec!["blender".to_string()], "normalised, and this computer is never a skill");
        assert!(job.notes_block.contains("blender/make a round object"), "{}", job.notes_block);
        assert_eq!(job.shown_notes, vec!["blender/make a round object".to_string()]);
    }
```


Also update `a_model_that_only_ever_replans_eventually_gives_up`: its prompt bound becomes
`<= 9` with the comment `// front door + plan + 6 replans + the learning turn`. Then run the whole
suite and fix any other prompt-count assertion that now counts one more prompt after a Done or
Failed. Say why in a comment.

- [ ] **Step 2: Implement**

`proto/src/lib.rs`, in `enum Event` after `Undone`:

```rust
    /// What the learning turn after a job kept (Phase 3 §6). `pending`: something waits for Keep.
    Learned { job_id: String, lines: Vec<String>, #[serde(default)] pending: bool },
```

and add `| Event::Learned { job_id, .. }` to `job_id()`.

`core/src/event.rs`, in `lines`:

```rust
        Event::Learned { lines, pending, .. } => {
            let mut v = lines.clone();
            if *pending { v.push("Say \"keep what you learned\" to keep what changes the machine, or \"discard what you learned\".".into()); }
            v
        }
```

`engine.rs`:

1. Next to `is_undo`:

```rust
/// Keep or throw away what the last learning turn left waiting (Phase 3 §5): fixed words, no model.
pub fn is_keep_learned(text: &str) -> bool { normalize(text) == "keep what you learned" }
pub fn is_discard_learned(text: &str) -> bool { normalize(text) == "discard what you learned" }
```

2. At the top of `handle_inner`, before `let open = self.open_job()?;`:

```rust
        if is_keep_learned(text) || is_discard_learned(text) {
            let keep = is_keep_learned(text);
            let c = self.store.conn();
            let n = if keep { crate::notes::keep_pending(c)? } else { crate::notes::discard_pending(c)? };
            let text = match (n, keep) { (0, _) => "There is nothing waiting to be kept.", (_, true) => "Kept what I learned.", (_, false) => "Discarded it." };
            self.emit(Event::Said { text: text.into() });
            return Ok(());
        }
```

3. A helper on `impl<M: Model> Engine<M>`:

```rust
    /// The tips a job starts with, fixed on the job (Phase 3 §3).
    fn give_tips(&self, job: &mut Job, skills: &[String]) -> Result<(), EngineError> {
        job.skills = skills.iter().map(|s| crate::notes::norm_notebook(s)).filter(|s| !s.is_empty() && s != crate::notes::THIS_COMPUTER).take(3).collect();
        let (block, shown) = crate::notes::for_job(self.store.conn(), &format!("{} {}", job.goal, job.request), &job.skills)?;
        job.notes_block = block;
        job.shown_notes = shown;
        Ok(())
    }
```

In the `Move::Start` arm, destructure `skills` and call `self.give_tips(&mut job, &skills)?;` right
after `job.request = text.to_string();`. In the `Move::Housekeep` arm, call
`self.give_tips(&mut job, &[])?;` right after `job.request = text.to_string();`. Both calls must
come before `save_job`.

4. The learning turn, called at the end of `finish` after `self.emit(ev);`:

```rust
        if state != State::Cancelled { self.learn(&job, state == State::Done); }
        Ok(())
```

```rust
    /// One model call after a job, never fatal (Phase 3 §4): the job is already over and said so.
    fn learn(&mut self, job: &Job, passed: bool) {
        let mv = match self.model.next_move(&prompt::learning_turn(job, passed)) {
            Ok(m) => m,
            Err(e) => { eprintln!("engine: no learning turn ({e})"); return }
        };
        let Move::Learn { entries, used, wrong, remove } = mv else { return };
        match crate::learn::apply(self.store.conn(), job, entries, &used, &wrong, &remove, passed) {
            Ok(l) if !l.lines.is_empty() => self.emit(Event::Learned { job_id: job.id.clone(), lines: l.lines, pending: l.pending }),
            Ok(_) => {}
            Err(e) => eprintln!("engine: what was learned could not be saved ({e})"),
        }
    }
```

`finish` currently moves `job` into nothing after the event (`job_id` is cloned first), so `&job`
is still valid there. If the borrow checker disagrees, clone `job` before building `ev`.
`State` must derive `PartialEq` (it does).

Also add `Move::Learn { .. }` to `run_turns`' out-of-turn arm so the match stays exhaustive:
`(_, Move::Reply { .. }) | (_, Move::Start { .. }) | (_, Move::Housekeep { .. }) | (_, Move::Learn { .. }) => Some("a job is running: use ask, plan, act, replan, done or give_up".to_string()),`
The grammar never allows it there. This arm is only for a runner that ignores the grammar.

- [ ] **Step 3: Commit, push, run the whole suite**

```bash
git add runtime
git commit -m "feat(engine): tips at the start of every job, a learning turn after it, keep/discard as fixed words"
git push && gh workflow run build.yml --ref skills && sleep 5 && gh run watch $(gh run list --branch skills --limit 1 --json databaseId -q '.[0].databaseId')
```

Expected: everything passes. The rail crate also matches on `Event`, so it may fail to compile
here until Task 8. If it does, add `Event::Learned { .. } => vec![]` to `Cards::apply` and
`Event::Learned { .. }` to the `None` arm of `busy_after` now, and let Task 8 replace them.

---

### Task 7: The service answers `skills` and `forget`

**Files:**
- Modify: `runtime/proto/src/lib.rs` (`NoteView`, `Notebook`, `Event::Skills`, `Request::Skills`, `Request::Forget`, `Writer::request`)
- Modify: `runtime/core/src/service.rs` (`db_path`, request arms)
- Modify: `runtime/core/src/bin/ai-os-engine.rs` (use `service::db_path()`)
- Modify: `runtime/core/src/event.rs` (`lines`: `Event::Skills { .. } => vec![]`)
- Test: `runtime/core/tests/service.rs`, `runtime/proto/src/lib.rs` tests

**Interfaces:**
- Produces:
  - `pub struct NoteView { pub topic: String, pub kind: String, pub text: String, pub uses: i64, pub failed: bool, pub needs_check: bool }`
  - `pub struct Notebook { pub name: String, pub entries: Vec<NoteView> }` (both Debug, Clone, PartialEq, Serialize, Deserialize)
  - `Event::Skills { notebooks: Vec<Notebook> }`: sent only to the client that asked.
  - `Request::Skills {}` (wire `{"skills":{}}`), `Request::Forget { notebook: String, topic: String }` (wire `{"forget":{"notebook":…,"topic":…}}`)
  - `Writer::request(&mut self, r: &Request) -> std::io::Result<()>`
  - `pub fn db_path() -> String` in `service.rs` (`AI_OS_DB`, default `/data/ai-os.db`)

- [ ] **Step 1: Failing tests**

In proto's tests module add:

```rust
    #[test]
    fn skills_requests_have_the_wire_shape_the_service_reads() {
        assert_eq!(serde_json::to_string(&Request::Skills {}).unwrap(), r#"{"skills":{}}"#);
        let f = Request::Forget { notebook: "blender".into(), topic: "bevel".into() };
        assert_eq!(serde_json::from_str::<Request>(&serde_json::to_string(&f).unwrap()).unwrap(), f);
        let e = Event::Skills { notebooks: vec![Notebook { name: "this computer".into(), entries: vec![NoteView { topic: "t".into(), kind: "technique".into(), text: "x".into(), uses: 2, failed: false, needs_check: true }] }] };
        assert!(serde_json::to_string(&e).unwrap().starts_with(r#"{"kind":"skills""#));
    }
```

In `runtime/core/tests/service.rs` add:

```rust
#[test]
fn the_skills_screen_is_answered_from_the_database_and_forget_deletes() {
    let dir = temp("skills");
    let db = dir.join("notes.db");
    std::env::set_var("AI_OS_DB", &db);
    {
        let c = rusqlite::Connection::open(&db).unwrap();
        aios_core::notes::init(&c).unwrap();
        aios_core::notes::put(&c, &aios_core::notes::Note { notebook: "this computer".into(), topic: "open a website".into(), kind: "technique".into(), text: "open_app firefox".into(), ..Default::default() }, None).unwrap();
    }
    let sock = start(&dir, vec![], Arc::new(Mutex::new(None)));
    let (mut r, mut w) = Client::connect(&sock).unwrap().split();
    w.request(&aios_proto::Request::Skills {}).unwrap();
    let Some(Event::Skills { notebooks }) = r.next_event() else { panic!("no skills event") };
    assert_eq!(notebooks[0].name, "this computer");
    assert_eq!(notebooks[0].entries[0].topic, "open a website");
    w.request(&aios_proto::Request::Forget { notebook: "this computer".into(), topic: "open a website".into() }).unwrap();
    let Some(Event::Skills { notebooks }) = r.next_event() else { panic!("no skills event after forget") };
    assert!(notebooks.is_empty(), "{notebooks:?}");
}
```

The core crate must list `rusqlite` under `[dev-dependencies]` for this test if integration tests
can't see the normal dependency. They can: integration tests get the package's `[dependencies]`.
So nothing to add.

- [ ] **Step 2: Implement**

`proto/src/lib.rs`: the two structs above `enum Event`; `Skills { notebooks: Vec<Notebook> },` in
`Event` after `Learned`; in `Request`:

```rust
    /// The Skills screen asks for the notebooks (Phase 3 §6); answered to that client only.
    #[serde(rename = "skills")] Skills {},
    /// The Skills screen's ✕ on one entry.
    #[serde(rename = "forget")] Forget { notebook: String, topic: String },
```

and in `impl Writer`: `pub fn request(&mut self, r: &Request) -> std::io::Result<()> { self.send(r) }`.

`service.rs`:

```rust
/// Where the engine's database is: its notebooks are read from here by the Skills screen.
pub fn db_path() -> String { std::env::var("AI_OS_DB").unwrap_or_else(|_| "/data/ai-os.db".into()) }

/// The notebooks as the Skills screen draws them, read on the client's own thread so the screen
/// opens while a job runs (Phase 3 §6).
fn skills_event(forget: Option<(&str, &str)>) -> Event {
    let read = || -> Result<Vec<aios_proto::Notebook>, crate::store::StoreError> {
        let c = rusqlite::Connection::open(db_path())?;
        c.busy_timeout(std::time::Duration::from_secs(5))?;
        crate::notes::init(&c)?;
        if let Some((nb, tp)) = forget { crate::notes::remove(&c, nb, tp)?; }
        Ok(crate::notes::all(&c)?.into_iter().map(|(name, notes)| aios_proto::Notebook {
            name,
            entries: notes.into_iter().map(|n| aios_proto::NoteView { topic: n.topic, kind: n.kind, text: n.text, uses: n.uses, failed: n.failed, needs_check: n.needs_check }).collect(),
        }).collect())
    };
    match read() { Ok(notebooks) => Event::Skills { notebooks }, Err(e) => Event::Error { text: format!("could not read the notebooks: {e}") } }
}
```

In the reader thread's `match`, before the `Err(_)` arm:

```rust
                    Ok(Request::Skills {}) => sh.send_to(id, &skills_event(None)),
                    Ok(Request::Forget { notebook, topic }) => sh.send_to(id, &skills_event(Some((&notebook, &topic)))),
```

`ai-os-engine.rs`: `let db = service::db_path();`.

`event.rs` `lines`: `Event::Skills { .. } => vec![],` (with the other silent ones).

- [ ] **Step 3: Commit, push, run the tests**

```bash
git add runtime
git commit -m "feat(service): the Skills screen reads and forgets entries straight from the database, even while a job runs"
# If the rail stops compiling on the new Event::Skills: add it to busy_after's None arm and
# `Event::Skills { .. } => vec![]` to Cards::apply now; Task 8 keeps them.
git push && gh workflow run build.yml --ref skills && sleep 5 && gh run watch $(gh run list --branch skills --limit 1 --json databaseId -q '.[0].databaseId')
```

---

### Task 8: The chat window — Learned lines, Keep/Discard, the Skills button, the guide

**Files:**
- Modify: `runtime/rail/src/cards.rs` (`CardKind::Done` gains `learned: Vec<String>`; `apply` handles `Learned`/`Skills`; `busy_after`)
- Modify: `runtime/rail/src/main.rs` (channel of `Request`; render learned lines; Skills button and window)
- Modify: `runtime/rail/src/guide.txt` (a Skills section)
- Test: `runtime/rail/tests/cards.rs`

**Interfaces:**
- Consumes: `Event::Learned`, `Event::Skills`, `Request::{Say, Skills, Forget}`, `Writer::request`.
- Produces: nothing other tasks use.

- [ ] **Step 1: Failing test** (append to `runtime/rail/tests/cards.rs`; reuse the file's `j()` helper for the job id)

```rust
#[test]
fn learned_lines_join_the_jobs_done_card_and_pending_ones_offer_keep_and_discard() {
    let mut cards = Cards::default();
    cards.apply(&Event::Done { job_id: j(), text: "done".into(), check: None, files: vec![], windows: vec![] });
    let ch = cards.apply(&Event::Learned { job_id: j(), lines: vec!["Learned, if you keep it: get gimp (this computer)".into()], pending: true });
    assert_eq!(ch, vec![Change::Updated(0)]);
    let CardKind::Done { learned, .. } = &cards.list[0].kind else { panic!() };
    assert_eq!(learned, &vec!["Learned, if you keep it: get gimp (this computer)".to_string()]);
    let says: Vec<&str> = cards.list[0].buttons.iter().map(|b| b.say.as_str()).collect();
    assert_eq!(says, vec!["undo", "keep what you learned", "discard what you learned"]);
    // A failed job's "marked as not working" has no Done card to join: it is a line of its own.
    let ch = cards.apply(&Event::Learned { job_id: "other".into(), lines: vec!["Marked as not working: this computer/x".into()], pending: false });
    assert_eq!(ch, vec![Change::Added(1)]);
    assert!(cards.apply(&Event::Skills { notebooks: vec![] }).is_empty());
}
```

- [ ] **Step 2: Implement `cards.rs`**

`CardKind::Done` becomes
`Done { text: String, check: Option<String>, files: Vec<ChangedFile>, windows: Vec<String>, learned: Vec<String> },`.
In `apply`'s `Done` arm, construct it with `learned: vec![]`. Add the arms:

```rust
            Event::Learned { job_id, lines, pending } => {
                let at = self.list.iter().rposition(|c| c.job_id.as_deref() == Some(job_id.as_str()) && matches!(c.kind, CardKind::Done { .. }));
                match at {
                    Some(i) => {
                        if let CardKind::Done { learned, .. } = &mut self.list[i].kind { learned.extend(lines.iter().cloned()); }
                        if *pending { self.list[i].buttons.extend([btn("Keep", "keep what you learned"), btn("Discard", "discard what you learned")]); }
                        vec![Change::Updated(i)]
                    }
                    None => self.push(Card { kind: CardKind::Said, text: lines.join("\n"), buttons: vec![], opens: vec![], thumbnails: vec![], job_id: None }),
                }
            }
            // The Skills screen is its own window (main.rs), not a card.
            Event::Skills { .. } => vec![],
```

`busy_after`: add `| Event::Learned { .. } | Event::Skills { .. }` to the `None` arm.
Fix the two existing test patterns in `rail/tests/cards.rs` if they bind every field. They use
`..`, so they should already be fine.

- [ ] **Step 3: Implement `main.rs`**

1. `use aios_proto::{ChangedFile, Client, Event, Notebook, Request};`
2. `net_thread` returns `Sender<Request>`: `let (say_tx, say_rx) = channel::<Request>();` and in its loop `Ok(r) => { if writer.request(&r).is_err() { break } }`.
3. Every place that sent a `String` on that channel now sends `Request::Say(…)`: the Stop button (`Request::Say("stop".into())`), the entry's `connect_activate`, the answer buttons in `render` (`let _ = s.send(Request::Say(t));`), and the card buttons (`let _ = s.send(Request::Say(t.clone()));`). `render`'s parameter becomes `say: &Sender<Request>`. Find them all with `grep -n "send(" runtime/rail/src/main.rs`. The `FromNet` senders are a different channel and stay as they are.
4. In `render`'s `CardKind::Done` arm bind `learned` and, after the window note:

```rust
            for l in learned { let w = text(l); w.add_css_class("dim"); b.append(&w); }
```

5. The Skills button, packed with Help (`header.pack_end(&skills_btn)` right after `header.pack_end(&help)`):

```rust
        let skills_btn = gtk::Button::with_label("Skills");
        skills_btn.set_tooltip_text(Some("What the AI has learned, and a way to delete it"));
```

and after `let say = net_thread(to_ui);`:

```rust
        let s_skills = say.clone();
        skills_btn.connect_clicked(move |_| { let _ = s_skills.send(Request::Skills {}); });
        let skills_win: Rc<RefCell<Option<gtk::Window>>> = Rc::default();
```

6. Where the event drain handles `FromNet::Event(ev)`, before `cards.apply(&ev)`:

```rust
                        if let Event::Skills { notebooks } = &ev { show_skills(&parent_for_skills, &skills_win, notebooks, &say_for_skills); continue; }
```

Clone `win`, `skills_win` and `say` into that closure as `parent_for_skills`, `skills_win`, and
`say_for_skills`, like the other clones there (`cards2`, `say2`…). If the drain is not a loop
with `continue`, wrap the rest of the arm in `else`.

7. The window:

```rust
/// The Skills screen (Phase 3 §6): each notebook, its entries with how often they helped, and a ✕
/// that deletes one. Rebuilt from every `skills` answer, so a delete shows at once.
fn show_skills(parent: &gtk::ApplicationWindow, slot: &Rc<RefCell<Option<gtk::Window>>>, notebooks: &[Notebook], say: &Sender<Request>) {
    let column = gtk::Box::new(gtk::Orientation::Vertical, 6);
    column.set_margin_start(12); column.set_margin_end(12); column.set_margin_top(12); column.set_margin_bottom(12);
    if notebooks.is_empty() {
        let l = gtk::Label::new(Some("Nothing learned yet. After a job that worked, what the AI learned shows here."));
        l.set_wrap(true); l.set_xalign(0.0); column.append(&l);
    }
    for nb in notebooks {
        let list = gtk::Box::new(gtk::Orientation::Vertical, 4);
        for n in &nb.entries {
            let row = gtk::Box::new(gtk::Orientation::Horizontal, 6);
            let mark = if n.failed { " — did not work last time" } else if n.needs_check { " — needs checking" } else { "" };
            let l = gtk::Label::new(Some(&format!("{}: {}\nused {} time{}{mark}", n.topic, n.text, n.uses, if n.uses == 1 { "" } else { "s" })));
            l.set_wrap(true); l.set_xalign(0.0); l.set_hexpand(true); l.set_selectable(true);
            row.append(&l);
            let x = gtk::Button::with_label("✕");
            x.set_tooltip_text(Some("Delete this"));
            let (s, notebook, topic) = (say.clone(), nb.name.clone(), n.topic.clone());
            x.connect_clicked(move |_| { let _ = s.send(Request::Forget { notebook: notebook.clone(), topic: topic.clone() }); });
            row.append(&x);
            list.append(&row);
        }
        let title = if nb.name == "this computer" { "This computer".to_string() } else { nb.name.clone() };
        let ex = gtk::Expander::builder().label(format!("{title} ({})", nb.entries.len())).child(&list).expanded(notebooks.len() == 1).build();
        column.append(&ex);
    }
    let scroll = gtk::ScrolledWindow::builder().child(&column).hscrollbar_policy(gtk::PolicyType::Never).build();
    let mut slot_ref = slot.borrow_mut();
    match slot_ref.as_ref().filter(|w| w.is_visible()) {
        Some(w) => w.set_child(Some(&scroll)),
        None => {
            let w = gtk::Window::builder().title("What the AI has learned").transient_for(parent).default_width(480).default_height(560).child(&scroll).build();
            w.present();
            *slot_ref = Some(w);
        }
    }
}
```

- [ ] **Step 4: The guide** (`runtime/rail/src/guide.txt`, a new section before the one about models)

```
<big><b>Skills: what it learns</b></big>

After a job that worked, the AI writes down what it would want to know straight away next time. It keeps these in notebooks:
• <b>This computer</b>: how your computer and its programs are used, like how to open a website. It reads this before every job.
• <b>A notebook per craft</b>, like "blender" or "web design": ways of doing things that worked, mistakes and how they were fixed, and what you liked. It reads one when a job is in that area.

The Done card says what it learned ("Learned: open a website"). If it learned something that changes the computer, like installing a program, it asks first: press <b>Keep</b> or <b>Discard</b>.
Press <b>Skills</b> at the top to read the notebooks and delete anything with ✕. A tip that stopped working is marked, and the AI fixes or removes it the next time.
```

- [ ] **Step 5: Commit, push, run the tests**

```bash
git add runtime/rail
git commit -m "feat(rail): Learned lines and Keep/Discard on the Done card, a Skills button to read and delete, the guide's Skills section"
git push && gh workflow run build.yml --ref skills && sleep 5 && gh run watch $(gh run list --branch skills --limit 1 --json databaseId -q '.[0].databaseId')
```

Expected: `package` builds the rail (GTK code compiles only there), and `test` passes.

---

### Task 9: Merge, release, and the owner's acceptance

**Files:** none new.

- [ ] **Step 1: Review.** Run the code-review skill on `master...skills`. Fix what it finds on the branch.
- [ ] **Step 2: Merge and graph.**

```bash
git checkout master && git merge --no-ff skills -m "Merge skills: phase 3 — notebooks of what worked, read at the start of every job, learned after it"
graphify update .
git push origin master
```

- [ ] **Step 3: Release** v0.7.0 once the master CI run is green:

```bash
git tag v0.7.0 && git -c credential.helper='!gh auth git-credential' push origin v0.7.0
```

- [ ] **Step 4: The owner's acceptance on his Ubuntu VM** (spec §8), in plain words for him:
  1. Update (the rail offers it at login, or re-run the install command).
  2. "Open the browser and go to example.org". It may be slow. The Done card should say "Learned: …", and **Skills** should show it under "This computer".
  3. "Open the browser and go to wikipedia.org". It should take far fewer steps, and the entry's "used" count should go up.
  4. Later: "make a round object in Blender", twice. A "blender" notebook should appear, and the second run should be shorter.

Whatever he reports becomes v0.7.x fixes.
