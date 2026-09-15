# Phase 0 findings

Started 2026-09-15. Throwaway distro `ai-os` on ALIEN. Spec v2 = `effba2c`.

| # | Unknown | Result | Evidence |
|---|---|---|---|
| U1 | GPU inside WSL | | |
| U2 | Full desktop on Windows | | |
| U3 | Accessibility on real apps | | |
| U4 | Focus stealing | | |
| U5 | Remembered screen permission | | |
| U6 | Snapshot disk with rollback | | |
| U7 | Local 8B: speed / parse rate / context | | |
| U8 | GNOME or KDE | | |

## Task 1: base
- WSL engine: 2.7.14.0, kernel 6.18.33.2-2, WSLg 1.0.73.2
- Distro name on the WSL list: `Ubuntu-26.04` (Ubuntu 26.04 LTS)
- Disk cap: 100 GB (`defaultVhdSize`), set by the user on 2026-09-15 instead of the spec's 200 GB
- os-release: `Ubuntu 26.04.1 LTS`
- systemd state: `running`
- Root disk after base: 98 GB total, 1.6 GB used
- No setup wizard appeared on first launch (root-run script, then `[user] default=ai`)
