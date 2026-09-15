# Phase 0 findings

Started 2026-09-15. Throwaway distro `ai-os` on ALIEN. Spec v2 = `effba2c`.

| # | Unknown | Result | Evidence |
|---|---|---|---|
| U1 | GPU inside WSL | PASS | `nvidia-smi` in the distro: RTX 5060 Laptop, 8151 MiB, driver 610.60; Ollama runs the model on it |
| U2 | Full desktop on Windows | | |
| U3 | Accessibility on real apps | | |
| U4 | Focus stealing | | |
| U5 | Remembered screen permission | | |
| U6 | Snapshot disk with rollback | | |
| U7 | Local 8B: speed / parse rate / context | PASS with a caveat | qwen3.5:9b: 35 tok/s, 1.25 s per tool call, 100% parse with and without grammar; **8k context is the most that stays fully on the GPU** (with q8_0 KV cache); 12k+ spills to CPU |
| U8 | GNOME or KDE | | |

## Task 1: base
- WSL engine: 2.7.14.0, kernel 6.18.33.2-2, WSLg 1.0.73.2
- Distro name on the WSL list: `Ubuntu-26.04` (Ubuntu 26.04 LTS)
- Disk cap: 100 GB (`defaultVhdSize`), set by the user on 2026-09-15 instead of the spec's 200 GB
- os-release: `Ubuntu 26.04.1 LTS`
- systemd state: `running`
- Root disk after base: 98 GB total, 1.6 GB used
- No setup wizard appeared on first launch (root-run script, then `[user] default=ai`)

## Task 2: GPU and model
- nvidia-smi inside WSL: NVIDIA GeForce RTX 5060 Laptop GPU, 8151 MiB, driver 610.60
- Runner: Ollama (trial only). Its installer needs `zstd`, which the 26.04 WSL image lacks; added to the gpu section.
- Model tag: `qwen3.5:9b` (6.6 GB, vision + tools + thinking). Chosen because the newer Qwen 3.6 / 3.8 families start at 27B and Gemma 4's smallest dense model is 12B; `qwen3-vl:8b` is the older alternative.
- Probe run with `think: false`, temperature 0, 8192 context:
  - tokens/s: **35.0**
  - tool-call parse rate with JSON-schema grammar: **1.0** (30/30); without grammar: **1.0** (30/30); 1.25 s per call either way
  - The grammar cost nothing in time and this model did not need it for these 10 tasks; keep it anyway (spec decision 13), the failure mode it prevents shows up on long jobs, not on 30 short calls.
- Context fit (`ollama ps` CPU/GPU split):
  - default KV cache: 4096 = 100% GPU; 8192 = 12% CPU; 12288 = 14% CPU; 16384 = 16% CPU
  - with `OLLAMA_FLASH_ATTENTION=1` + `OLLAMA_KV_CACHE_TYPE=q8_0`: **8192 = 100% GPU**; 12288 = 12% CPU; 16384 = 14% CPU
  - **Largest context fully on GPU: 8192** (with the q8_0 cache). VRAM at that point: 6.4 GB of 8.
- Consequence for Phase 1: the per-step context budget on 8 GB cards is 8k for a 9B model, not the 16k assumed in spec 4.3. Either the budget shrinks to 8k, or a 4B-class model is used when more context is needed. The evaluation (spec 6) should include both.
- Vision check: pending the screenshot from Task 5.
