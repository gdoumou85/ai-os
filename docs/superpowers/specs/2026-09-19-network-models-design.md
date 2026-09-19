# Models on the home network — design

2026-09-19. Approved in conversation with the owner.

## Why

The AI OS can only use an Ollama model, and only at an address typed in by hand. The owner's GPU is
out of reach of the Ubuntu VM (VirtualBox passes no GPU through), so the model has to run on
another machine on the home network. The machines on his network run Ollama (the Windows laptop)
and LM Studio (the laptop and a second PC). Addresses on a home network change when the router
hands out new ones.

## What gets built

### 1. The finder: `ai-os-find`

A new binary in `runtime/core` (`src/bin/ai-os-find.rs`, with the logic in `src/find.rs` so the
engine can call it too).

- **Which network.** It opens a UDP socket "connected" to a public address (nothing is sent) and
  reads the local address the OS picked. That gives the address of the default-route interface. The
  scan covers that address's /24 (`a.b.c.1`–`a.b.c.254`), plus `127.0.0.1`.
  `ponytail:` /24 on the default route only. Larger subnets and multi-NIC hosts need a real
  interface list.
- **Which doors.** 11434 (Ollama) and 1234 (LM Studio) on every address. It runs a TCP connect
  with a 300 ms timeout from a pool of threads (std only, no new dependency), so a full scan
  finishes in a few seconds.
- **What it asks.** An open 11434 gets `GET /api/tags` → Ollama, with its model names. An open 1234
  gets `GET /v1/models` (with `Authorization: Bearer <key>` when a key is given) → LM Studio, with
  its model ids. A 401 is reported as "needs a key".
- **Output.** One line per model, tab-separated: `kind\turl\tmodel` where kind is `ollama` or
  `openai`. A runner that needs a key prints `openai\turl\t-\tneeds-key`. The output is sorted, so
  the numbering is stable between runs.

### 2. The engine speaks LM Studio

`model.rs` gains `OpenAiModel { url, model, key }` → `POST {url}/v1/chat/completions`:
- `response_format: { type: "json_schema", json_schema: { name: "move", strict: true, schema:
  <narrowed schema> } }`. This is the same grammar forcing Ollama gets through `format`.
- `temperature: 0`, and the same two messages. The answer is read from
  `choices[0].message.content`.
- LM Studio fixes the context length when it loads a model, so the request can't set it.
  `context_tokens()` stays 8192, and the installer tells the person to load the model with at
  least 8192.

The engine binary picks the kind from `AI_OS_MODEL_KIND` (`ollama` is the default, `openai`) and reads
`AI_OS_MODEL_KEY` when present. Both the Ollama and the LM Studio client sit behind one
`RemoteModel` that the engine holds (the engine is generic over one `Model` type).

### 3. "When lost": finding the model again

When a request fails because it **can't connect** (not an HTTP error status, and not a bad answer),
`RemoteModel` runs the finder once. If the same kind and the same model name answer at another
address, it switches to that address in memory, logs the change, and retries the request once. If
nothing turns up, the original error goes up and the chat shows it as it does today. The new
address isn't written anywhere. The next start after a restart scans again if it has to.

### 4. The installer and the check

- `install.sh` with no `--model-url`: runs `bin/ai-os-find` from the tarball. It prints a numbered
  list, plus one more line: "install Ollama on this machine". It reads the choice from `/dev/tty`,
  because the installer arrives through `curl | bash`, so stdin is the script. A chosen LM Studio
  that needs a key → `read -rs` for the key from `/dev/tty`. The installer re-runs the finder with
  the key to confirm the key works and get the model list.
- `--model-url URL [--model NAME]` still works with no questions asked. The kind is found out by
  asking the URL (`/api/tags` answers → ollama, otherwise openai). `--model-key KEY` is new, for
  runs with no prompt.
- **Where the settings live.** Kind and URL go in the unit as `Environment=` lines as today. The
  key goes in `~/.config/ai-os/model.env` (mode 0600), which the unit reads with
  `EnvironmentFile=-%h/.config/ai-os/model.env`.
- `check.sh` asks the right door for the kind: `/api/tags` for ollama, `/v1/models` with the key for
  openai.

## Tests (run by CI; there is no local Rust toolchain)

- `OpenAiModel`: the request body (the schema is narrowed and present, temperature 0), parsing a
  good answer, a non-2xx status, a non-JSON body. Reuses the `respond_once` fake server.
- The finder: fake Ollama and fake LM Studio listeners on 127.0.0.1 ports (the probe takes a port
  list, so tests use ephemeral ports). A 401 gives needs-key. A closed port gives nothing.
- `RemoteModel` when lost: the first address refuses the connection, and a fake finder result
  points at a live fake server → the move comes back from the second address. With no match → the
  original error.
- The installer and the check: `bash -n` in CI, then a live run in the owner's Ubuntu VM.

## Not in this

Scanning beyond a /24, picking a model automatically, a settings screen for switching models (re-run
the installer), and keys for Ollama.

## Open issue carried in

On the owner's first VM install, systemd reported `Exec format error` for
`/usr/local/bin/ai-os-engine`, although the released binary is a valid x86-64 ELF. It isn't
diagnosed yet. The first live run of this work has to check it (`file`, `ls -l` on the installed
binary).
