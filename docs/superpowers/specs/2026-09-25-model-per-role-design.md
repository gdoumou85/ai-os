# Model per role (agentic cloud, stage 3)

The owner, 2026-09-25: helpers should be "special finetuned subagents" per kind of task, running
on several free providers at once. Stage 2 gave each role its own brief. This stage gives each role
the model that suits it, and keeps slow models out of the way (NVIDIA's kimi-k3 took about two
minutes to start answering "hi").

## What it does

1. **Speeds, kept from real work.** Every cloud answer, from the main AI (`cloud::Pooled::ask`) or a
   helper, is timed, and the time is written to `~/.config/ai-os/speeds.tsv`
   (`url<TAB>model<TAB>seconds`, halfway between the last time and this one). No test requests:
   OpenRouter's free models allow about 50 answers a day.
2. **A pick per role** (`aios_proto::rank`, shared by the engine and the card). A helper tries these
   models in order:
   - the owner's pick for its role (`~/.config/ai-os/roles.tsv`, `role<TAB>url<TAB>model`);
   - then models whose name fits the role (`aios_proto::fits`), fastest first, with an untimed
     model counted as 30 s. Coding and debugging want a coder; reasoning and review a thinking
     model; design and art a big all-round model (helpers have no screen, so seeing does not help);
   - then the rest, spread from account *i* as in stage 2;
   - models slower than `SLOW` (120 s an answer) last.

   Helpers of one role start on different fitting models, so they still spread.
3. **Helpers' models card** (Model → Cloud accounts → Helpers' models). It asks each OpenAI-style
   account which models its key opens and shows:
   - the main AI's model and its speed;
   - each role's first model, its speed, and whether it was picked automatically or by the owner.

   **Change** lists every model with its speed. **Automatic** takes the owner's pick out.
4. **Thinking shows.** `watch` now hears `Heard { thinking, writing }`, so the status line reads
   "Thinking… N words" until the answer starts.

## Left out

- The main AI's model is not switched on its own. It stays the owner's (Use this one), and the card
  shows its speed.
- No speed tests on demand, since each test spends free allowance. Times fill in as the AI works.
