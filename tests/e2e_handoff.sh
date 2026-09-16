#!/bin/sh
set -eu

command -v tmux >/dev/null 2>&1 || {
	echo "SKIP handoff: tmux unavailable"
	exit 0
}
command -v python3 >/dev/null 2>&1 || {
	echo "SKIP handoff: python3 unavailable"
	exit 0
}

# Exercise tmux's real direct-vs-grouped attachment semantics on a private
# socket. No SSH alias, user configuration or pre-existing session is used.
python3 - <<'PY'
import os
import pty
import shutil
import signal
import subprocess
import tempfile
import time

root = tempfile.mkdtemp(prefix='hmux-e2e-handoff-', dir='/tmp')
binary = shutil.which('tmux')
base = [binary, '-S', root + '/tmux.sock', '-f', '/dev/null']
env = dict(os.environ, TERM='xterm-256color')
env.pop('TMUX', None)
env.pop('TMUX_PANE', None)
children = []

def tmux(*args):
    return subprocess.check_output(base + list(args), env=env, stderr=subprocess.PIPE).decode().strip()

def client(*args):
    pid, fd = pty.fork()
    if pid == 0:
        os.execve(binary, base + list(args), env)
    children.append((pid, fd))
    return pid

def clients():
    value = tmux('list-clients', '-F', '#{client_pid}')
    return set(map(int, value.split())) if value else set()

def wait_for(predicate, message):
    deadline = time.monotonic() + 5
    while time.monotonic() < deadline:
        if predicate():
            return
        time.sleep(0.05)
    raise AssertionError(message)

try:
    original = 'hmux-e2e-original'
    view = 'hmux-e2e-grouped'
    identity = tmux('new-session', '-d', '-P', '-F', '#{session_id}:#{session_created}', '-s', original, 'sleep 120')
    target = identity.split(':')[0]
    options = tmux('show-options', '-t', target)
    panes = tmux('list-panes', '-s', '-t', target, '-F', '#{pane_id}')
    first = client('attach-session', '-t', target)
    wait_for(lambda: clients() == {first}, 'first client failed to attach')
    second = client('attach-session', '-d', '-t', target)
    wait_for(lambda: clients() == {second}, 'direct handoff did not replace first client')
    shared = client('attach-session', '-t', target)
    wait_for(lambda: clients() == {second, shared}, 'shared direct attach displaced a client')
    view_id = tmux('new-session', '-d', '-P', '-F', '#{session_id}', '-s', view, '-t', target)
    tmux('set-option', '-t', view_id, 'status', 'off')
    grouped = client('attach-session', '-t', view_id)
    wait_for(lambda: clients() == {second, shared, grouped}, 'grouped view displaced an original client')
    os.kill(grouped, signal.SIGTERM)
    wait_for(lambda: clients() == {second, shared}, 'grouped client did not close independently')
    tmux('kill-session', '-t', view_id)
    actual_identity = tmux('display-message', '-p', '-t', target, '#{session_id}:#{session_created}')
    actual_options = tmux('show-options', '-t', target)
    assert actual_identity == identity, ('original identity changed', identity, actual_identity)
    assert actual_options == options, ('original options changed', options, actual_options)
    assert tmux('list-panes', '-s', '-t', target, '-F', '#{pane_id}') == panes, 'original panes changed'
    assert clients() == {second, shared}, 'original clients changed'
    print('handoff-ok shared-ok grouped-close-preserves-original-ok')
finally:
    subprocess.run(base + ['kill-server'], env=env, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    for pid, fd in children:
        try:
            os.kill(pid, signal.SIGTERM)
        except ProcessLookupError:
            pass
        os.close(fd)
        os.waitpid(pid, 0)
    shutil.rmtree(root)
PY
