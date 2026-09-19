# Models on the home network — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** The AI OS finds Ollama and LM Studio runners on the home network. It uses either one, with
a key when LM Studio asks for it, and finds the runner again when its address changes.

**Architecture:** A finder module (`find.rs`) scans the /24 on ports 11434/1234 and lists
models. It is a library for the engine and a binary (`ai-os-find`) for the installer. `OllamaModel`
becomes `RemoteModel { kind, url, model, key, finder }`, which speaks Ollama `/api/chat` or
OpenAI-style `/v1/chat/completions`, and runs the finder once when its address can't be reached. The
installer shows the finder's list and writes the kind, the URL and the key into the user's unit.

**Tech Stack:** Rust (std threads, ureq 2, serde_json). No new dependencies. Bash for the installer.
CI in an ubuntu:26.04 container. There's no local Rust toolchain, so "run the tests" means pushing
the branch and running `gh workflow run build.yml --ref network-models`, then
`gh run watch`.

**Spec:** `docs/superpowers/specs/2026-09-19-network-models-design.md`

## Global Constraints

- Ports: Ollama `11434`, LM Studio `1234`. Scan: default-route /24 plus `127.0.0.1`, a 300 ms connect timeout.
- Finder output: `kind\turl\tmodel`, or `kind\turl\t-\tneeds-key`. The kind is `ollama` or `openai`.
- Unit env: `AI_OS_MODEL_URL`, `AI_OS_MODEL_KIND`. The key is `AI_OS_MODEL_KEY` in `~/.config/ai-os/model.env` (0600), read via `EnvironmentFile=-%h/.config/ai-os/model.env`.
- The key never appears on a command line (argv is world-readable in `ps`).
- The installer reads answers from `/dev/tty`, because stdin is the script under `curl | bash`.
- Context stays 8192. LM Studio models must be loaded with ≥ 8192.

---

### Task 1: The fake server and the finder

**Files:**
- Modify: `runtime/core/src/testing.rs` (append `serve`, `json_response`, `closed_port`)
- Create: `runtime/core/src/find.rs`
- Create: `runtime/core/src/bin/ai-os-find.rs`
- Modify: `runtime/core/src/lib.rs` (add `pub mod find;`), `runtime/core/Cargo.toml` (add the `[[bin]]`)

**Interfaces:**
- Produces: `find::{Kind, Found, OLLAMA_PORT, LMSTUDIO_PORT, home_hosts, scan, ask}`.
  - `enum Kind { Ollama, OpenAi }` with `as_str()` / `parse()`
  - `struct Found { url: String, kind: Kind, model: Option<String> }`, where `None` means it needs a key
  - `scan(hosts: &[Ipv4Addr], ollama_port: u16, openai_port: u16, key: Option<&str>) -> Vec<Found>`
  - `ask(kind: Kind, url: &str, key: Option<&str>) -> Vec<Found>`
- Produces: `testing::{serve(response: String, times: usize) -> SocketAddr, json_response(status, body) -> String, closed_port() -> u16}`

- [ ] **Step 1:** Append the fake server to `testing.rs`. It reads the headers and then the
  body up to `Content-Length`, so a big POST isn't cut off by a reset.
- [ ] **Step 2:** Write `find.rs` with its tests: an Ollama listing that drops embedding models,
  an LM Studio listing, a 401 giving needs-key, and a scan that probes a fake plus a closed port
  and finds exactly one model. `home_hosts()` starts with 127.0.0.1.
- [ ] **Step 3:** Write the `ai-os-find` binary: `ai-os-find [--url URL]`, with the key taken only
  from `AI_OS_MODEL_KEY`. `--url` asks that one URL as Ollama first, then as LM Studio. It exits 1
  when nothing is found.
- [ ] **Step 4:** Commit: `feat(find): list the model runners on the home network`.

### Task 2: RemoteModel — LM Studio, and finding the runner again

**Files:**
- Modify: `runtime/core/src/model.rs` (`OllamaModel` → `RemoteModel`; `openai_body`, `parse_openai`, `post_json`, the lost path)
- Modify: `runtime/core/src/bin/ai-os-engine.rs`, `runtime/core/tests/live_{1c,1d,2a,primes}.rs` (rename only)

**Interfaces:**
- Consumes: `find::{Kind, Found, scan, home_hosts, OLLAMA_PORT, LMSTUDIO_PORT}`, `testing::{serve, json_response, closed_port}`
- Produces: `RemoteModel { kind: Kind, url: RefCell<String>, model: String, key: Option<String>, finder: Box<dyn Fn() -> Vec<Found>> }`
  with `RemoteModel::at(kind, url, model)` (no key, a finder that finds nothing), `RemoteModel::local(model)`
  (Ollama at 127.0.0.1:11434), and `RemoteModel::from_env(model)` (reads `AI_OS_MODEL_URL`,
  `AI_OS_MODEL_KIND` and `AI_OS_MODEL_KEY`, with the real finder). Also `openai_body(model, prompt)`
  and `parse_openai(resp)`.

- [ ] **Step 1:** Tests first: the openai body is grammar-forced (`response_format.json_schema.strict`,
  the narrowed `oneOf`, temperature 0), `parse_openai` good and bad, an LM Studio round trip through
  `serve`, a runner that moved is found again (the closed port, then a finder that points at the fake,
  and `url` is updated), a runner that is gone gives `ModelError::Http`, and `from_env` reads the kind
  and the key. The existing Ollama tests move from `respond_once` to `testing::serve`.
- [ ] **Step 2:** Implement. The lost path runs only on `ureq::Error::Transport` with
  `ErrorKind::ConnectionFailed`. An HTTP status error or a bad answer never triggers a scan.
- [ ] **Step 3:** Rename `OllamaModel` → `RemoteModel` in the engine binary and the live tests.
- [ ] **Step 4:** Commit: `feat(model): LM Studio's OpenAI-style door, and the runner found again when it moves`.

### Task 3: Installer, unit, check, release

**Files:**
- Modify: `install/install.sh` (flags `--model-key`, the picker, key prompt, `model.env`, installs `ai-os-find`)
- Modify: `install/ai-os-engine.service.in` (`EnvironmentFile=-%h/.config/ai-os/model.env`)
- Modify: `install/check.sh` (asks the runner's own door by kind, with the key from `model.env`)
- Modify: `install/make-release.sh` (ship `ai-os-find`)
- Modify: `README.md` (install section: no flags → it lists what it found)

- [ ] **Step 1:** `install.sh`: parse `--model-key`. Leave the model empty by default. The native
  branch defaults it to `qwen3.5:9b`. Add `find_models` (runs `$here/bin/ai-os-find` with the key in
  its environment) and `pick <offer-native> <lines…>`, which prints a numbered list to `/dev/tty`,
  reads the choice from `/dev/tty`, and prints the chosen line or `native`. The needs-key → read the
  key hidden → re-ask with it → pick the model. Write `AI_OS_MODEL_KIND` next to `AI_OS_MODEL_URL`.
  Write or remove `model.env` (umask 077).
- [ ] **Step 2:** `check.sh`: `openai` → `curl -H @<(printf 'Authorization: Bearer %s\n' "$key") $url/v1/models`,
  `ollama` → `/api/tags` as today.
- [ ] **Step 3:** `bash -n` all scripts (can be done locally in Git Bash).
- [ ] **Step 4:** Commit: `feat(install): pick a model runner found on the network, with its key`.

### Task 4: CI on the branch

- [ ] Push `network-models`. Run `gh workflow run build.yml --ref network-models`, then `gh run watch`. Fix
  compile and test failures until green. The tarball artifact then goes to the owner for the VM test,
  where the first check is the `Exec format error` carried over from the spec.
