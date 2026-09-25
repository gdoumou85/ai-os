# Helpers: the main AI hands work to role helpers running at once (stage 2 of agentic cloud)

**Date:** 2026-09-25 · **Status:** the owner said "you start stage 2" on the design in chat · **Release:** v0.14.0

## What the owner asked for

"Parallel work, orchestrator agent and special subagents for specific kinds of tasks (one for
coding, one for design, one for reasoning, one for review, one for debugging, one for artistic
creation)"; "multi agenting should be using multiple different cloud providers at the same time
as well and move to the next model if a model refuses to work". Stage 1 (native tools, v0.13.0)
made cloud models use their actions; stage 3 (choosing a model per role) comes after this.

## Design

1. **One new action, `delegate`.** `helpers: [{role, task}]`, 1–6 of them. Roles: `coding`,
   `design`, `reasoning`, `review`, `debugging`, `art`. It runs in the engine's own lane. The
   main AI stays the orchestrator: it splits the work, writes each task so it stands alone, and
   reads what comes back; the helpers never talk to the owner.
2. **At the same time, on different providers.** Each helper is a thread with its own cloud
   model: helper *i* starts on account *i* (cloud.tsv order, OpenAI-style accounts), so helpers
   spread over OpenRouter and NVIDIA at once. When its model fails — an error, a used-up
   allowance, an answer it cannot read — it moves to the next account's model, then to the other
   models those keys open, and says so on the card. Up to 8 models are tried.
3. **What a helper is.** A small loop of its own (`core::helpers`): a brief for its role, its task,
   the project folder, and only the machine hand's actions (commands, files, web, background
   programs) as native tools, plus `reply` to hand its result back. The desktop and the screen
   stay with the main AI: there is one of each. At most 40 moves; Stop stops every helper.
   History lives in memory only: the owner's chat is the main AI's.
4. **On the card.** Each helper's action is a step line: `coding · <model>: wrote app.py`; a
   switch of model is a line too. The status line says how many helpers are working.
5. **Back to the main AI.** One result: each helper's role, model, whether it finished, and its
   reply (cut to 1500 characters each). The main AI then reviews, combines, or sends more work.
6. **Only where it works.** `delegate` is offered (in the tools, the grammar and the brief) only
   while Cloud is on with an OpenAI-style account. Home models never see it.

## Not in this stage

Choosing a model per role (stage 3); helpers using the desktop or the screen; helpers starting
helpers; a separate rail view per helper; keeping helper transcripts after the turn.

## Tests

`helpers::run` with two fake providers: both helpers run and report; a helper whose first model
fails moves to the next and says so; a helper's desktop action is refused. `delegate` with no
cloud account is refused with a reason. The schema leaves `delegate` out for home models and
keeps only machine actions for helpers.
