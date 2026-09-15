#!/usr/bin/env bash
# U6 probe: snapshot, change, roll back, prove the change is gone. Run as root.
set -euo pipefail
live=/data/live; snaps=/data/snapshots
echo "version 1" >"$live/file.txt"
t0=$(date +%s%N)
btrfs subvolume snapshot -r "$live" "$snaps/before" >/dev/null
t1=$(date +%s%N)
echo "version 2" >"$live/file.txt"; echo junk >"$live/junk.txt"
# rollback = swap the live subvolume for a writable copy of the snapshot
t2=$(date +%s%N)
btrfs subvolume snapshot "$snaps/before" "$live.new" >/dev/null
mv "$live" "$live.old" && mv "$live.new" "$live"
btrfs subvolume delete "$live.old" >/dev/null
t3=$(date +%s%N)
[ "$(cat "$live/file.txt")" = "version 1" ] || { echo "FAIL: file not restored"; exit 1; }
[ ! -e "$live/junk.txt" ] || { echo "FAIL: junk survived rollback"; exit 1; }
btrfs subvolume delete "$snaps/before" >/dev/null
echo "PASS snapshot_ms=$(( (t1-t0)/1000000 )) rollback_ms=$(( (t3-t2)/1000000 ))"
