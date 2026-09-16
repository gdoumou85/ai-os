#!/usr/bin/env python3
"""THROWAWAY: does the duplicated 'Recent exchange' line flip the front door from start to reply?"""
import json, sys, urllib.request
sys.path.insert(0, "/mnt/c/Users/gdoum/Desktop/projects/ai-os/trial")
from importlib import import_module
d = import_module("loop-dryrun") if False else None  # keep this file self-contained
exec(open("/mnt/c/Users/gdoum/Desktop/projects/ai-os/trial/loop-dryrun.py").read().split("def ollama")[0])  # SYSTEM, SCHEMA, MODEL

def narrowed():
    s = json.loads(json.dumps(SCHEMA))
    s["oneOf"] = [o for o in s["oneOf"] if o["properties"]["move"]["enum"][0] in ("reply", "start")]
    return s

SORT=False
def call(user):
    fmt = json.loads(json.dumps(narrowed(), sort_keys=True)) if SORT else narrowed()
    body = {"model": MODEL, "stream": False, "think": False, "format": fmt,
            "options": {"temperature": 0.0, "num_ctx": 8192},
            "messages": [{"role": "system", "content": SYSTEM}, {"role": "user", "content": user}]}
    req = urllib.request.Request("http://127.0.0.1:11434/api/chat", json.dumps(body).encode(), {"Content-Type": "application/json"})
    return json.loads(json.load(urllib.request.urlopen(req, timeout=300))["message"]["content"])

MSG = "Start a new project called primes: make a Python script that prints the first ten prime numbers, one per line, and prove it runs. Decide the details yourself."
LEGAL = ("Legal moves now: reply (just talk) or start (new work: give project, new_project, description, goal, creative, understood). "
         "Pick an existing project name when the user means one. Set creative=true only if the user said to decide yourself.")
variants = {
    "A engine-as-built (message duplicated in Recent exchange)":
        f"Standing instructions:\n(none)\n\nProjects:\n(none yet)\n\nRecent exchange:\nuser: {MSG}\n\n{LEGAL}\n\nUser says: {MSG}",
    "B no duplication (recent exchange empty)":
        f"Standing instructions:\n(none)\n\nProjects:\n(none yet)\n\nRecent exchange:\n\n\n{LEGAL}\n\nUser says: {MSG}",
    "C no duplication + explicit rule":
        f"Standing instructions:\n(none)\n\nProjects:\n(none yet)\n\nRecent exchange:\n\n\n{LEGAL} "
        f"If the user wants something made, changed or done, answer start and put what you understood in its understood field; reply is only for conversation that needs no work.\n\nUser says: {MSG}",
}
variants = {"B no duplication (recent exchange empty)": variants["B no duplication (recent exchange empty)"]}
for SORT in (False, True):
  print("sorted keys:", SORT)
  for name, user in variants.items():
    for i in range(2):
        mv = call(user)
        print(f"{name} | run {i+1}: move={mv['move']} " + (f"creative={mv.get('creative')} project={mv.get('project')}" if mv["move"] == "start" else f"text={mv.get('text','')[:90]}"), flush=True)
