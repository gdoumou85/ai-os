# Full access — design (v0.8.0)

**Date:** 2026-09-19. **Decided by:** the owner, after v0.7.1 ("the design is wrong. The AI should
have FULL access to everything. If it kills the OS it kills it — that's why we run it in a VM").

## Decision

The AI is no longer confined. It runs as the owner with passwordless root, the whole network and
every folder, and never asks before acting. The VM is the safety net: the owner takes VirtualBox
snapshots. This reverses the design's confinement decisions (sandbox, risk rules, approvals, the
admin menu, Undo) from `2026-09-15-ai-os-design.md` onward.

Owner's answers:
- **Asking first:** never. No approval card, no risk rule, no declined-action memory.
- **Mouse stop:** removed. The screen fallback keeps working while the person uses the mouse.
- **Undo:** removed. No Undo button, undo log or per-job project snapshot.

## Approach

The engine keeps running as the owner (a user service), because the desktop hand needs the
owner's session bus, AT-SPI and screencast; running it as root would break them. Root comes from
`sudo`: `install.sh` grants the owner `NOPASSWD: ALL`, and the model uses `sudo` in its commands.

Rejected: keeping the admin helper and marking everything Auto (all the confinement code stays,
guarding nothing); running the engine as root (breaks the desktop hand).

## What goes

1. **The command sandbox.** `run_command` runs the argv directly as the owner in the job's folder,
   with the network on. `sandbox-run`, `wrong_hand`, `hidden_note`, `absolute_program` and the
   `ai-sandbox` user's role in running commands go.
2. **The admin lane.** `ai-os-admin`, `admin.rs`, the fetch allowlist and the actions `install`,
   `remove`, `service`, `fetch_packages` and `make_dir` go: the model does them with
   `run_command` (`sudo apt-get install -y …`, `sudo systemctl …`, `mkdir -p …`).
3. **Folder limits.** `read_file`, `write_file` and `edit_file` take any absolute path, or a path
   relative to the job's folder. A write the owner may not make goes through `sudo -n tee`.
4. **Asking.** `rules.rs` classification, the approval card, approved/declined action memory and
   the risk words on `press`/`screen_click` go. `http_post` goes (it never worked; `curl` covers it).
   The `asks_permission` send-back stays: the model is told to act, not ask.
5. **Undo.** The Undo button, the undo log, `reverse`, project snapshots and "predates undo" lines
   go. `/data` stays as installed, so existing VMs keep working.
6. **The mouse stop.** `took_over` and the job cancel it triggers go.
7. **The prompt's "you cannot" lines.** Rewritten: the model has root through `sudo`, the network,
   and every folder; programs on the desktop are still worked through their controls.

## What stays

The engine's loop guards (an immediate repeat of a step that worked; the step, failure, rejection,
replan and done-gate budgets), the answer timeout, Skills, the Stop button, the desktop and screen
hands, `set_setting projects_root` (any absolute path now).

## Help guide

`rail/src/guide.txt` loses Undo and the approval questions and says the AI has full access and that
VirtualBox snapshots are the way back.

## Testing

Unit tests on CI follow the removals; tests that pinned the confinement are deleted, not bent.
The owner's check on the VM: "open Firefox", "install cowsay", "write hello into /etc/aios-test"
each finish with no question.
