# Cloud models use native tools (stage 1 of agentic cloud)

**Date:** 2026-09-25 · **Status:** approved by the owner in chat ("these are good", "yes") · **Release:** v0.13.0

## Why

Cloud models are trained to act through an API's `tools` (function calling). AI OS gave them its
actions only as words in the brief plus a JSON answer format, and free OpenRouter models answered
"no file or command tools are available" / "this turn exposed no filesystem action tool" on a
machine they have full access to. 18 of OpenRouter's 20 free models take `tools`; 7 take
`response_format`. Tested 2026-09-25 through this PC's Ollama (`nemotron-3-super:cloud`, OpenAI
endpoint): given `read_file`/`run_command`/`reply` as tools and "read the blueprint", it called
`read_file` at once.

Stage 2 (orchestrator + parallel role helpers) and stage 3 (a model per role) build on this;
they get their own specs.

## Scope

OpenAI-style cloud accounts only (OpenRouter, NVIDIA — the owner's providers; "ollama cloud is
bad"). Home runners and Ollama-kind accounts keep the JSON `format` path, which works for them.

## Design

1. **Tools from the one schema.** `schema::tools(allowed, no_screen)` turns `MOVE_SCHEMA` into
   OpenAI tools: each allowed move except `act` is a tool named after it (its properties minus
   `move`); `act` becomes one tool per action kind (`read_file`, `run_command`, …, its properties
   minus `kind`, plus `thought`), screen kinds dropped when the model cannot see. Nothing new to
   keep in step: a new move or action appears as a tool by itself.
2. **Request.** `RemoteModel.tools` (set by the cloud pool for OpenAI-kind accounts) sends
   `tools` and `tool_choice: "auto"` instead of `response_format`, and the plain prompt (the
   schema-in-words of `format_spelled` is for the JSON path only).
3. **Answer.** `read_stream` gathers streamed `delta.tool_calls` (name once, arguments in
   pieces, by index) and turns the first call back into the move JSON the rest of the engine
   already reads: a move tool → `{"move": name, …args}`, an action tool →
   `{"move":"act","thought":…,"action":{"kind": name, …args}}`. Argument pieces count as words
   on the status line and are never "blank" for the stuck guard. With no tool call, text that is
   not a move is a plain reply.
4. **Fallback.** A provider that refuses tools (an HTTP error naming tools, e.g. OpenRouter's
   "No endpoints found that support tool use") is asked again on the JSON path, and that
   account's model stays on it for the rest of the process.
5. **Brief.** One line: when no action does what is needed, make one — write a script or
   install a program, then run it. Full access means the AI can build its own tools.

## Not in this stage

Ollama-kind tools; replaying history as native `tool_calls`/`tool` messages (history stays text:
the move JSON, then the result); `tool_choice: "required"` (not every free provider takes it —
`promises_work` already sends back a reply that only talks about work).

## Tests

`schema::tools` (act expands, screen dropped, narrowing kept, no `move`/`kind` parameter); a
streamed tool call split over chunks reads as `Act ReadFile`; a move tool reads as its move;
plain text in tools mode is a reply; the pool falls back to JSON after a tools refusal and stays
there.
