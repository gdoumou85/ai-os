#!/usr/bin/env bash
# Start an invisible KDE/kwin_wayland session for user ai (virtual output, no display anywhere),
# bring up the AT-SPI stack by hand, launch LibreOffice into it, then run the a11y + action probes.
# Mirror of headless-session.sh but for KWin, to compare KDE vs GNOME for U3/U4/U8.
set -e
export XDG_RUNTIME_DIR=/run/user/1000 DBUS_SESSION_BUS_ADDRESS=unix:path=/run/user/1000/bus
sudo loginctl enable-linger ai >/dev/null
systemctl --user stop ai-headless-shell.service 2>/dev/null || true   # free the a11y bus from GNOME
systemctl --user stop ai-headless-kwin.service 2>/dev/null || true
systemctl --user reset-failed ai-headless-kwin.service 2>/dev/null || true
: >~/kwin.log
# kwin_wayland virtual backend: a headless compositor with one virtual output, no GPU, no screen
systemd-run --user --unit=ai-headless-kwin \
  --setenv=XDG_SESSION_TYPE=wayland --setenv=XDG_CURRENT_DESKTOP=KDE \
  --setenv=QT_QPA_PLATFORM=wayland --setenv=QT_ACCESSIBILITY=1 --setenv=QT_LINUX_ACCESSIBILITY_ALWAYS_ON=1 \
  --property=StandardOutput=append:/home/ai/kwin.log --property=StandardError=append:/home/ai/kwin.log \
  kwin_wayland --virtual --width 1600 --height 900 --no-lockscreen
sleep 8
display=$(grep -oE "wayland-[0-9]+" ~/kwin.log | tail -1)
[ -z "$display" ] && display=wayland-1
cat >~/kde.env <<EOF
export XDG_RUNTIME_DIR=/run/user/1000
export DBUS_SESSION_BUS_ADDRESS=unix:path=/run/user/1000/bus
export WAYLAND_DISPLAY=$display
export XDG_SESSION_TYPE=wayland XDG_CURRENT_DESKTOP=KDE
export QT_QPA_PLATFORM=wayland QT_ACCESSIBILITY=1 QT_LINUX_ACCESSIBILITY_ALWAYS_ON=1
export GTK_A11Y=1 GNOME_ACCESSIBILITY=1 ACCESSIBILITY_ENABLED=1
EOF
source ~/kde.env
# AT-SPI is not tied to a desktop: start the bus launcher + registry by hand (GNOME does this for us normally)
pgrep -u ai -f at-spi-bus-launcher >/dev/null || (/usr/libexec/at-spi-bus-launcher --launch-immediately >/dev/null 2>&1 &)
sleep 1
pgrep -u ai -f at-spi2-registryd >/dev/null || (/usr/libexec/at-spi2-registryd >/dev/null 2>&1 &)
sleep 2
systemctl --user set-environment XDG_CURRENT_DESKTOP=KDE XDG_SESSION_TYPE=wayland WAYLAND_DISPLAY=$display QT_ACCESSIBILITY=1
dbus-update-activation-environment --systemd XDG_CURRENT_DESKTOP=KDE XDG_SESSION_TYPE=wayland WAYLAND_DISPLAY=$display QT_ACCESSIBILITY=1
echo "WAYLAND_DISPLAY=$display"
echo "== kwin alive? =="; systemctl --user is-active ai-headless-kwin.service
echo "== screencast/remote-desktop names on the bus (KDE tier-3 fallback) =="
busctl --user list 2>/dev/null | grep -iE 'a11y|kde.KWin.ScreenShot|org.freedesktop.portal|RemoteDesktop|ScreenCast' || true
qdbus6 --session 2>/dev/null | grep -iE 'kwin|ScreenShot|portal' || qdbus --session 2>/dev/null | grep -iE 'kwin|ScreenShot|portal' || true
