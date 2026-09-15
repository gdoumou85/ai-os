#!/usr/bin/env python3
"""U1/U7 probe. Usage: model_probe.py <model-tag> [screenshot.png]
Prints JSON: tokens_per_s, parse_rate_schema, parse_rate_free, gpu_share_at_16k, vision_ok."""
import base64, json, subprocess, sys, time, urllib.request

URL = "http://127.0.0.1:11434/api/chat"
MODEL = sys.argv[1]
TOOL_SCHEMA = {
    "type": "object",
    "properties": {
        "tool": {"type": "string", "enum": ["run_command", "read_file", "write_file", "click", "type_text", "ask_user"]},
        "args": {"type": "object"},
        "why": {"type": "string"},
    },
    "required": ["tool", "args", "why"],
}
SYSTEM = ("You are the core of an AI operating system. Reply with ONE JSON object only: "
          '{"tool": <one of run_command, read_file, write_file, click, type_text, ask_user>, '
          '"args": {...}, "why": "<one sentence>"}. No prose outside the JSON.')
TASKS = [
    "List the files in the user's home folder.",
    "Open the file /etc/hostname and tell me the machine name.",
    "Create a file notes.txt in the workspace containing the word hello.",
    "Press the Save button in the open LibreOffice window.",
    "Type 'quarterly report' into the document title field.",
    "The user asked to delete all photos. What do you do first?",
    "Install the package htop.",
    "Check whether port 8000 is in use.",
    "Rename report_final.docx to report_v2.docx in the workspace.",
    "Find out how much free disk space there is.",
]


def chat(messages, num_ctx=8192, fmt=None, images=None):
    # think=False: the per-step path the core will use; thinking mode is measured separately if wanted
    body = {"model": MODEL, "messages": messages, "stream": False, "think": False,
            "options": {"num_ctx": num_ctx, "temperature": 0}}
    if fmt is not None:
        body["format"] = fmt
    if images:
        body["messages"][-1]["images"] = images
    req = urllib.request.Request(URL, json.dumps(body).encode(), {"Content-Type": "application/json"})
    with urllib.request.urlopen(req, timeout=600) as r:
        return json.load(r)


def parse_ok(text):
    try:
        obj = json.loads(text)
        return (isinstance(obj, dict) and obj.get("tool") in TOOL_SCHEMA["properties"]["tool"]["enum"]
                and isinstance(obj.get("args"), dict))
    except Exception:
        return False


def parse_rate(fmt):
    ok, per_call = 0, []
    for task in TASKS:
        for _ in range(3):  # 30 calls per mode
            t = time.time()
            resp = chat([{"role": "system", "content": SYSTEM}, {"role": "user", "content": task}], fmt=fmt)
            per_call.append(time.time() - t)
            ok += parse_ok(resp["message"]["content"])
    return ok / (len(TASKS) * 3), round(sum(per_call) / len(per_call), 2)


out = {"model": MODEL}
resp = chat([{"role": "user", "content": "Explain in about 300 words how to install a package on Ubuntu."}])
out["tokens_per_s"] = round(resp["eval_count"] / (resp["eval_duration"] / 1e9), 1)
out["parse_rate_schema"], out["s_per_call_schema"] = parse_rate(TOOL_SCHEMA)
out["parse_rate_free"], out["s_per_call_free"] = parse_rate(None)
chat([{"role": "user", "content": "hi"}], num_ctx=16384)
ps = subprocess.run(["ollama", "ps"], capture_output=True, text=True).stdout
line = next((l for l in ps.splitlines() if MODEL in l), "")
out["gpu_share_at_16k"] = " ".join(line.split()[-4:-2]) if line else "not loaded"
out["ollama_ps"] = ps.strip()
out["vision_ok"] = None
if len(sys.argv) > 2:
    img = base64.b64encode(open(sys.argv[2], "rb").read()).decode()
    resp = chat([{"role": "user", "content": "What application is shown in this screenshot? One line."}], images=[img])
    out["vision_answer"] = resp["message"]["content"].strip()
    out["vision_ok"] = len(out["vision_answer"]) > 0
print(json.dumps(out, indent=1))
