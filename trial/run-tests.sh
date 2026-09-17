#!/usr/bin/env bash
# Runs cargo inside the distro from the Windows-mounted repo. Usage: run-tests.sh test -p executor
cd /mnt/c/Users/gdoum/Desktop/projects/ai-os/runtime && cargo "$@" 2>&1 | grep -vE '^\s+(Compiling|Checking|Finished|Running)' | tail -60
