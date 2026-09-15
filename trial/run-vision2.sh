#!/usr/bin/env bash
source ~/session.env
echo "== shot.png"; ls -la ~/probes/shot.png; file ~/probes/shot.png
echo "== raw vision request error body"
python3 - <<'PY'
import base64, json, urllib.request
img = base64.b64encode(open("/home/ai/probes/shot.png","rb").read()).decode()
body = {"model":"qwen3.5:9b","stream":False,
        "messages":[{"role":"user","content":"What is shown in this screenshot? One line.","images":[img]}]}
req = urllib.request.Request("http://127.0.0.1:11434/api/chat", json.dumps(body).encode(), {"Content-Type":"application/json"})
try:
    r = urllib.request.urlopen(req, timeout=600)
    print("OK:", json.load(r)["message"]["content"].strip())
except urllib.error.HTTPError as e:
    print("HTTP", e.code, e.read().decode()[:300])
PY
echo "== does this tag report vision?"
curl -s http://127.0.0.1:11434/api/show -d '{"model":"qwen3.5:9b"}' | python3 -c 'import sys,json; d=json.load(sys.stdin); print("families:",d.get("details",{}).get("families")); print("caps:",d.get("capabilities"))'
