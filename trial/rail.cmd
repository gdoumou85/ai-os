@echo off
rem WORKSHOP ONLY: start the chat rail as a Windows window through WSLg.
rem A GTK window created before WSLg's RDP client has attached (the first seconds of a cold WSL
rem boot) never gets another frame callback: it shows, then neither moves nor takes keys. The
rem seat appears when the client attaches, which Weston logs, so wait for that line (30 s cap).
rem No $ in the command: wsl feeds it through the login shell, which would expand it first.
wsl -d ai-os -u ai -- bash -c "timeout 30 bash -c 'until grep -qs convert_rdp_keyboard /mnt/wslg/weston.log; do sleep 0.5; done'; exec ai-os-rail"
