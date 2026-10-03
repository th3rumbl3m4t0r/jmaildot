#!/bin/bash
# Runs maildot in a headless GNOME compositor (mutter) for scripted UI checks.
#   tests/headless.sh start [profile-dir]   # starts dbus + mutter + maildot (debug socket /tmp/maildot-ui.sock)
#   tests/headless.sh stop
#   tests/dbg.py 'tree'                     # talk to it
set -u
RUN=/tmp/maildot-headless
BIN=${MAILDOT_BIN:-$(cd "$(dirname "$0")/.." && pwd)/target/debug/maildot}
case "${1:-}" in
start)
    "$0" stop >/dev/null 2>&1
    mkdir -p "$RUN" && chmod 700 "$RUN"
    export XDG_RUNTIME_DIR=$RUN
    eval "$(dbus-launch --sh-syntax)"
    echo "export DBUS_SESSION_BUS_ADDRESS='$DBUS_SESSION_BUS_ADDRESS'" > "$RUN/env"
    echo "$DBUS_SESSION_BUS_PID" > "$RUN/dbus.pid"
    setsid mutter --headless --wayland --no-x11 --wayland-display=maildot-test --virtual-monitor 1600x1000 > "$RUN/mutter.log" 2>&1 < /dev/null &
    echo $! > "$RUN/mutter.pid"
    for i in $(seq 1 50); do [ -S "$RUN/maildot-test" ] && break; sleep 0.2; done
    [ -S "$RUN/maildot-test" ] || { echo "mutter did not start"; tail "$RUN/mutter.log"; exit 1; }
    export WAYLAND_DISPLAY=maildot-test GDK_BACKEND=wayland
    export MAILDOT_HOME=${2:-/tmp/maildot-ui} MAILDOT_DEBUG_SOCK=/tmp/maildot-ui.sock MAILDOT_NO_KEYRING=1
    export GSK_RENDERER=${GSK_RENDERER:-cairo}
    rm -f /tmp/maildot-ui.sock
    setsid "$BIN" > "$RUN/maildot.log" 2>&1 < /dev/null &
    echo $! > "$RUN/maildot.pid"
    for i in $(seq 1 50); do [ -S /tmp/maildot-ui.sock ] && break; sleep 0.2; done
    [ -S /tmp/maildot-ui.sock ] && echo "running (log $RUN/maildot.log)" || { echo "maildot did not start"; tail -20 "$RUN/maildot.log"; exit 1; }
    ;;
stop)
    for p in maildot mutter dbus; do
        [ -f "$RUN/$p.pid" ] && kill "$(cat "$RUN/$p.pid")" 2>/dev/null
        rm -f "$RUN/$p.pid"
    done
    sleep 0.5
    pkill -f "mutter --headless --wayland --no-x11 --wayland-display=maildot-test" 2>/dev/null
    echo stopped
    ;;
*)
    echo "usage: $0 start [profile] | stop"
    ;;
esac
