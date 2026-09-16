#!/usr/bin/env python3
"""THROWAWAY spike (2026-09-16): can qwen3.5:9b drive the Phase 1b loop?

Sends the plan's exact system text + job-turn prompt to Ollama with the real move schema,
applies the loop's rules in a few dozen lines, and performs moves: file ops in the demo
workspace (as the executor's user would), run_command through the real 1a sandbox binary.
Nothing here ships. Usage (inside the distro):
  python3 trial/loop-dryrun.py --mode creative [--think-plan] [--plant-error] [--max 20]
"""
import argparse, json, os, re, subprocess, sys, time, urllib.request

REPO = "/mnt/c/Users/gdoum/Desktop/projects/ai-os"
WS = "/data/jobs/demo"          # the 1a demo binary's fixed workspace
EXEC = f"{REPO}/runtime/target/debug/executor"
MODEL = "qwen3.5:9b"

SYSTEM = """You are the AI that runs this computer for its user. You answer with exactly one JSON move.
Rules:
- You act only through moves; the executor runs them and reports back. Never claim something ran unless the report says so.
- You cannot know the full scope of what the user imagines. When starting work, ask what you need to know (1-3 questions) unless the job is in creative mode; then decide yourself.
- Say what you understood before you act.
- Work from the project's BLUEPRINT.md: read it to find what to change and where. After each change, update BLUEPRINT.md in place (replace lines, never pile on; keep it as small as possible). Create it first for a new project.
- Edit code in place with edit_file (quote the exact passage). Use write_file only for new files. Use read_file with from_line/lines to read the part you need.
- A step that failed once will fail again. Read the reason and do something different, or replan. Only give_up as a last resort, and say what was missing.
- You are done only when a check proves it: done must carry a check action whose success is the proof.
- If something is worth remembering, write it down (BLUEPRINT.md, or `remember` for a standing instruction). You will not see this conversation again."""

ACTION = {"oneOf": [
    {"type": "object", "properties": {"kind": {"enum": ["run_command"]}, "argv": {"type": "array", "items": {"type": "string"}, "minItems": 1}}, "required": ["kind", "argv"], "additionalProperties": False},
    {"type": "object", "properties": {"kind": {"enum": ["read_file"]}, "path": {"type": "string"}, "from_line": {"type": "integer"}, "lines": {"type": "integer"}}, "required": ["kind", "path"], "additionalProperties": False},
    {"type": "object", "properties": {"kind": {"enum": ["write_file"]}, "path": {"type": "string"}, "contents": {"type": "string"}}, "required": ["kind", "path", "contents"], "additionalProperties": False},
    {"type": "object", "properties": {"kind": {"enum": ["edit_file"]}, "path": {"type": "string"}, "find": {"type": "string"}, "replace": {"type": "string"}}, "required": ["kind", "path", "find", "replace"], "additionalProperties": False},
    {"type": "object", "properties": {"kind": {"enum": ["http_post"]}, "url": {"type": "string"}, "body": {"type": "string"}}, "required": ["kind", "url", "body"], "additionalProperties": False},
]}
SCHEMA = {"$defs": {"action": ACTION}, "oneOf": [
    {"type": "object", "properties": {"move": {"enum": ["reply"]}, "text": {"type": "string"}, "remember": {"type": "string"}}, "required": ["move", "text"], "additionalProperties": False},
    {"type": "object", "properties": {"move": {"enum": ["start"]}, "project": {"type": "string"}, "new_project": {"type": "boolean"}, "description": {"type": "string"}, "goal": {"type": "string"}, "creative": {"type": "boolean"}, "understood": {"type": "string"}, "remember": {"type": "string"}}, "required": ["move", "project", "new_project", "description", "goal", "creative", "understood"], "additionalProperties": False},
    {"type": "object", "properties": {"move": {"enum": ["ask"]}, "questions": {"type": "array", "items": {"type": "string"}, "minItems": 1, "maxItems": 3}}, "required": ["move", "questions"], "additionalProperties": False},
    {"type": "object", "properties": {"move": {"enum": ["plan"]}, "steps": {"type": "array", "items": {"type": "string"}, "minItems": 1, "maxItems": 8}}, "required": ["move", "steps"], "additionalProperties": False},
    {"type": "object", "properties": {"move": {"enum": ["act"]}, "step": {"type": "integer"}, "action": {"$ref": "#/$defs/action"}}, "required": ["move", "step", "action"], "additionalProperties": False},
    {"type": "object", "properties": {"move": {"enum": ["replan"]}, "steps": {"type": "array", "items": {"type": "string"}, "minItems": 1, "maxItems": 8}, "why": {"type": "string"}}, "required": ["move", "steps", "why"], "additionalProperties": False},
    {"type": "object", "properties": {"move": {"enum": ["done"]}, "summary": {"type": "string"}, "check": {"$ref": "#/$defs/action"}}, "required": ["move", "summary", "check"], "additionalProperties": False},
    {"type": "object", "properties": {"move": {"enum": ["give_up"]}, "reason": {"type": "string"}, "missing": {"type": "string"}}, "required": ["move", "reason", "missing"], "additionalProperties": False},
]}


def ollama(system, user, think):
    body = {"model": MODEL, "stream": False, "think": think, "format": SCHEMA,
            "options": {"temperature": 0.0, "num_ctx": 8192},
            "messages": [{"role": "system", "content": system}, {"role": "user", "content": user}]}
    req = urllib.request.Request("http://127.0.0.1:11434/api/chat", json.dumps(body).encode(), {"Content-Type": "application/json"})
    t = time.time()
    resp = json.load(urllib.request.urlopen(req, timeout=600))
    dt = time.time() - t
    content = resp["message"]["content"]
    try:
        return json.loads(content), dt, resp.get("eval_count", 0), resp.get("prompt_eval_count", 0)
    except json.JSONDecodeError:
        return {"move": "__bad__", "raw": content}, dt, 0, 0


# --- a tiny stand-in for the engine's worker: files in-process, commands via the sandbox binary
def inside(path):
    full = os.path.realpath(os.path.join(WS, path))
    return full if full.startswith(WS + "/") else None


def perform(action):
    k = action["kind"]
    if k == "run_command":
        out = subprocess.run([EXEC, json.dumps(action)], capture_output=True, text=True)
        # The binary prints one "RAN ok=… detail=…" / "BLOCKED: …" record whose detail spans lines.
        text = out.stdout
        i = max(text.find("RAN "), text.find("BLOCKED:"))
        line = text[i:].strip() if i >= 0 else text.strip() + out.stderr[-300:]
        if line.startswith("BLOCKED:"):
            return None, line
        ok = line.startswith("RAN ok=true")
        detail = line.split("detail=", 1)[-1] if "detail=" in line else line
        return ok, (detail[:500] if ok else detail[-500:])
    if k == "http_post":
        return None, "BLOCKED: sends data off the machine"
    p = inside(action["path"])
    if not p:
        return False, "path escapes workspace"
    if k == "write_file":
        os.makedirs(os.path.dirname(p), exist_ok=True)
        open(p, "w").write(action["contents"])
        return True, "written"
    if k == "read_file":
        if not os.path.exists(p):
            return False, "cannot resolve path"
        text = open(p).read()
        if action.get("from_line") or action.get("lines"):
            start = max(1, action.get("from_line", 1)); n = min(200, action.get("lines", 200))
            return True, "".join(f"{i+1}: {l}\n" for i, l in enumerate(text.splitlines()) if start - 1 <= i < start - 1 + n)
        return True, text[:2000]
    if k == "edit_file":
        if not os.path.exists(p):
            return False, "cannot resolve path"
        text = open(p).read(); c = text.count(action["find"])
        if c == 0: return False, "find text not found — re-read the file and quote it exactly"
        if c > 1: return False, f"find text occurs in {c} places — include more surrounding lines so it is unique"
        open(p, "w").write(text.replace(action["find"], action["replace"], 1))
        return True, "edited"
    return False, "no hand for this action"


def summarise(steps):
    if not steps: return "(nothing done yet)"
    out = []
    for i, s in enumerate(steps):
        if i + 6 < len(steps):
            out.append(f"step {i+1}: {s['action']['kind']} {'ok' if s['ok'] else 'failed'}")
        else:
            out.append(f"step {i+1} (plan step {s['plan_step']}): {json.dumps(s['action'])} -> {'ok' if s['ok'] else 'failed'}: {s['detail']}")
    return "\n".join(out)


def job_turn(job):
    answers = "\n".join(f"- {q} -> {a}" for q, a in job["answers"]) or "(none)"
    plan = "\n".join(f"{i+1}. {s}" for i, s in enumerate(job["plan"])) or "(no plan yet)"
    bp_path = os.path.join(WS, "BLUEPRINT.md")
    bp = open(bp_path).read()[:3000] if os.path.exists(bp_path) else "(no blueprint yet — create BLUEPRINT.md with write_file before changing anything else)"
    hint = {"asking": "Legal moves now: ask (1-3 questions) or plan (if you have no questions).",
            "planning": "Legal moves now: plan. Give 2-8 short steps in plain words."}.get(job["state"],
            "Legal moves now: act (one action for the plan step it serves), replan, done (with a check action), give_up (say what was missing).")
    note = f"\n\nNote from the executor: {job['note']}" if job.get("note") else ""
    mode = "creative (do not ask; decide yourself)" if job["creative"] else "ask"
    return (f"Standing instructions:\n(none)\n\nProject: {job['project']} (its folder is the working directory)\nGoal: {job['goal']}\nMode: {mode}\n"
            f"What you told the user you understood: {job['understood']}\n\nUser's answers:\n{answers}\n\nPlan:\n{plan}\n\nBLUEPRINT.md:\n{bp}\n\n"
            f"Steps so far:\n{summarise(job['steps'])}{note}\n\n{hint}")


def is_bp(action):
    return action["kind"] in ("write_file", "edit_file") and os.path.basename(action["path"]) == "BLUEPRINT.md"


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--mode", default="creative", choices=["ask", "creative"])
    ap.add_argument("--think-plan", action="store_true", help="thinking on for asking/planning turns")
    ap.add_argument("--plant-error", action="store_true", help="corrupt the first .py written, once")
    ap.add_argument("--max", type=int, default=25)
    a = ap.parse_args()

    subprocess.run(["rm", "-rf", WS]); os.makedirs(WS)
    subprocess.run(["chgrp", "ai-sandbox", WS]); subprocess.run(["chmod", "2770", WS])

    log = {"args": vars(a), "turns": []}
    def rec(**kw):
        log["turns"].append(kw); print(json.dumps(kw)[:400], flush=True)

    # 1. front door
    user_msg = ("Make a Python script that prints the first ten prime numbers, one per line, and prove it runs."
                + (" Decide the details yourself." if a.mode == "creative" else ""))
    fd = ("Standing instructions:\n(none)\n\nProjects:\n(none yet)\n\nRecent exchange:\n\n\nLegal moves now: reply (just talk) or start "
          "(new work: give project, new_project, description, goal, creative, understood). Pick an existing project name when the user means one. "
          f"Set creative=true only if the user said to decide yourself.\n\nUser says: {user_msg}")
    mv, dt, ev, pv = ollama(SYSTEM, fd, False)
    rec(turn=0, state="front_door", move=mv, secs=round(dt, 1), out_tokens=ev, in_tokens=pv)
    if mv.get("move") != "start":
        print("front door did not start a job; stopping"); json.dump(log, open(f"{REPO}/trial/loop-dryrun.last.json", "w"), indent=1); return
    creative = bool(mv.get("creative")) if a.mode == "creative" else False
    job = {"project": "primes", "goal": mv["goal"], "creative": creative or a.mode == "creative", "understood": mv["understood"],
           "state": "planning" if (creative or a.mode == "creative") else "asking", "answers": [], "plan": [], "steps": [],
           "failed": [], "rejections": 0, "note": None, "last_code": 0, "last_bp": 0}
    planted = False

    for turn in range(1, a.max + 1):
        if len(job["steps"]) >= 25:
            rec(turn=turn, result="FAILED: step cap"); break
        think = a.think_plan and job["state"] in ("asking", "planning")
        mv, dt, ev, pv = ollama(SYSTEM, job_turn(job), think)
        m = mv.get("move"); st = job["state"]; rejected = None; result = None
        if m == "ask" and job["creative"]:
            rejected = "this job is in creative mode: decide yourself instead of asking"
        elif m == "ask":
            job["answers"].append((" / ".join(mv["questions"]), "Python 3, plain print, one per line; decide everything else yourself."))
            job["state"] = "planning" if not job["plan"] else "working"; result = "answered"
        elif m == "plan" and st in ("asking", "planning"):
            job["plan"] = mv["steps"]; job["state"] = "working"
        elif m == "plan":
            rejected = "you already have a plan; use replan to change it"
        elif st != "working":
            rejected = "give a plan first"
        elif m == "replan":
            job["plan"] = mv["steps"]; job["note"] = f"plan revised because: {mv['why']}"
        elif m in ("act", "done"):
            action = mv["action"] if m == "act" else mv["check"]
            key = json.dumps(action, sort_keys=True)
            if m == "done" and job["last_code"] > job["last_bp"]:
                rejected = "update BLUEPRINT.md for what you changed before saying done"
            elif key in job["failed"]:
                earlier = next(s["detail"] for s in reversed(job["steps"]) if json.dumps(s["action"], sort_keys=True) == key)
                rejected = f"that exact action already failed with: {earlier} — work around it or replan"
            else:
                ok, detail = perform(action)
                if ok is None:
                    result = f"WAITING FOR OK: {detail}"; rec(turn=turn, state=st, move=mv, secs=round(dt, 1), out_tokens=ev, in_tokens=pv, result=result); break
                ps = mv.get("step", len(job["plan"])) if m == "act" else max(1, len(job["plan"]))
                job["steps"].append({"plan_step": ps, "action": action, "ok": ok, "detail": detail})
                job["rejections"] = 0; job["note"] = None
                if ok:
                    if is_bp(action): job["last_bp"] = len(job["steps"])
                    elif action["kind"] in ("write_file", "edit_file"): job["last_code"] = len(job["steps"])
                    if a.plant_error and not planted and action["kind"] == "write_file" and action["path"].endswith(".py"):
                        p = inside(action["path"]); t = open(p).read()
                        if "print" in t:
                            open(p, "w").write(t.replace("print", "prnt", 1)); planted = True; result = "PLANTED: print->prnt"
                else:
                    job["failed"].append(key)
                    if sum(1 for s in job["steps"] if s["plan_step"] == ps and not s["ok"]) >= 3:
                        result = "FAILED: 3 different failures on one step"; rec(turn=turn, state=st, move=mv, secs=round(dt, 1), result=result, detail=detail); break
                if m == "done":
                    if ok:
                        result = f"DONE: {mv['summary']} | check output: {detail}"; rec(turn=turn, state=st, move=mv, secs=round(dt, 1), out_tokens=ev, in_tokens=pv, result=result); break
                    job["note"] = "your check failed — read its output above, fix the work, then say done again with a check"
                    result = "check failed"
                else:
                    result = ("ok: " if ok else "FAIL: ") + detail[:200]
        elif m == "give_up":
            result = f"GAVE UP: {mv['reason']} | missing: {mv['missing']}"; rec(turn=turn, state=st, move=mv, secs=round(dt, 1), result=result); break
        else:
            rejected = "a job is running: use ask, plan, act, replan, done or give_up"
        if rejected:
            job["rejections"] += 1; job["note"] = f"your last move was rejected: {rejected}"; result = f"REJECTED: {rejected}"
            if job["rejections"] >= 2:
                rec(turn=turn, state=st, move=mv, secs=round(dt, 1), result=result + " (2 in a row → FAILED)"); break
        rec(turn=turn, state=st, move=mv, secs=round(dt, 1), out_tokens=ev, in_tokens=pv, result=result)

    json.dump(log, open(f"{REPO}/trial/loop-dryrun.last.json", "w"), indent=1)
    print("\nworkspace:", subprocess.run(["ls", "-la", WS], capture_output=True, text=True).stdout)
    bp = os.path.join(WS, "BLUEPRINT.md")
    if os.path.exists(bp): print("BLUEPRINT.md:\n" + open(bp).read())


if __name__ == "__main__":
    main()
