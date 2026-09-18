@echo off
rem WORKSHOP ONLY: build (or rebuild) the desktop edition VM. See make-vm.ps1.
powershell -NoProfile -ExecutionPolicy Bypass -File "%~dp0make-vm.ps1" %*
