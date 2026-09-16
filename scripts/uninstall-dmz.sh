#!/bin/sh
set -eu

STAMP="$(date -u +%Y%m%dT%H%M%SZ)-$$"
TRASH="$HOME/.local/share/hmux-uninstall-$STAMP"
PREFIX="${HMUX_CONTROL_ROOT:-$HOME/.local/share/hmux-control}"
case "$PREFIX" in
"" | / | "$HOME" | "$HOME/.local" | "$HOME/.local/share")
	echo "hmux: refusing unsafe control root" >&2
	exit 1
	;;
esac
systemctl --user disable --now hmux-reconcile.timer hmux-health.timer 2>/dev/null || true
mkdir -p "$TRASH"
for unit in hmux-reconcile.service hmux-reconcile.timer hmux-health.service hmux-health.timer; do
	path="$HOME/.config/systemd/user/$unit"
	[ -e "$path" ] && mv "$path" "$TRASH/"
done
if [ -e "$HOME/.local/bin/hmux-control" ]; then
	mv "$HOME/.local/bin/hmux-control" "$TRASH/bin-hmux-control"
fi
if [ -e "$PREFIX" ] || [ -L "$PREFIX" ]; then
	mv "$PREFIX" "$TRASH/control-data"
fi
systemctl --user daemon-reload
echo "DMZ files moved to recoverable location: $TRASH"
