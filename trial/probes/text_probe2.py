#!/usr/bin/env python3
"""Throwaway 2a probe, second pass. Writer on WSLg: three menu-paste attempts with longer waits.
Text editor: after insert, does the window title show the modified mark and can the Save-as flow see it?"""
import json, os, subprocess, sys, time
import pyatspi
sys.path.insert(0, os.path.dirname(__file__))
env = dict(os.environ, GNOME_ACCESSIBILITY="1", SAL_USE_VCLPLUGIN="gtk3")
desk = pyatspi.Registry.getDesktop(0)

def find_app(name):
    for app in desk:
        if name in (app.name or "").lower():
            try:
                if app.childCount: return app
            except Exception: pass

def walk(acc, limit=6000):
    stack=[acc]
    while stack and limit>0:
        n=stack.pop(); limit-=1; yield n
        try: stack.extend(n[i] for i in range(min(n.childCount,300)))
        except Exception: pass

def words(app):
    for n in walk(app):
        try:
            if n.getRoleName()=="label" and "word" in (n.name or ""): return n.name
        except Exception: pass

def menu_item(app, menu_name, sub):
    for n in walk(app):
        try:
            if n.getRoleName()=="menu" and (n.name or "")==menu_name:
                n.queryAction().doAction(0); time.sleep(2)
                for i in range(min(n.childCount,40)):
                    it=n[i]
                    if sub in (it.name or "").lower(): return it
        except Exception: pass

out={}
mode=sys.argv[1]
if mode=="writer":
    p=subprocess.Popen(["libreoffice","--writer","--norestore"],env=env,stdout=subprocess.DEVNULL,stderr=subprocess.DEVNULL)
    app=None
    for _ in range(45):
        time.sleep(1); app=find_app("soffice")
        if app: break
    time.sleep(6)
    tries=[]
    for i in range(3):
        r=subprocess.run(["wl-copy",f"P{i}-ai-os "],env=env); time.sleep(2)
        it=menu_item(app,"Edit","paste")
        if it is None: tries.append("no item"); continue
        it.queryAction().doAction(0); time.sleep(3)
        tries.append({"wlcopy":r.returncode,"words":words(app)})
    out["writer_menu_paste"]=tries
    p.terminate()
else:
    p=subprocess.Popen(["gnome-text-editor","--new-window"],env=env,stdout=subprocess.DEVNULL,stderr=subprocess.DEVNULL)
    app=None
    for _ in range(30):
        time.sleep(1); app=find_app("text-editor")
        if app: break
    time.sleep(4)
    title_before=[w.name for w in app]
    ed=None
    for n in walk(app):
        try:
            if n.getRoleName()=="text" and n.getState().contains(pyatspi.STATE_EDITABLE): ed=n; break
        except Exception: pass
    ed.queryEditableText().insertText(0,"hello from ai-os",16); time.sleep(2)
    out["editor"]={"title_before":title_before,"title_after":[w.name for w in app],
                   "readback":ed.queryText().getText(0,-1)}
    p.terminate()
print(json.dumps(out))
