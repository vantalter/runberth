#!/usr/bin/env python3
"""End-to-end check of the Linux tray: menu, clicks, and notifications.
Run: dbus-run-session -- python3 app/tests/tray_linux.py target/debug/runberth-app
Stands in for the tray host, the notification server, ssh, and GitHub's API.
Requires python3-gi, only for this test.
"""
import http.server
import json
import os
import signal
import subprocess
import sys
import tempfile
import threading
import time
from pathlib import Path

from gi.repository import Gio, GLib

binary = os.path.abspath(sys.argv[1])
bus = Gio.bus_get_sync(Gio.BusType.SESSION, None)
registrations = []
notifications = []
failures = []


def check(condition, message):
    if not condition:
        failures.append(message)
        print("FAIL:", message, flush=True)


def registered(connection, sender, path, interface, method, parameters, invocation):
    registrations.append((sender, parameters.unpack()[0]))
    invocation.return_value(None)


watcher = Gio.DBusNodeInfo.new_for_xml("""<node>
<interface name="org.kde.StatusNotifierWatcher">
<method name="RegisterStatusNotifierItem"><arg type="s" direction="in"/></method>
</interface></node>""")
bus.register_object("/StatusNotifierWatcher", watcher.interfaces[0], registered, None, None)


def notified(connection, sender, path, interface, method, parameters, invocation):
    notifications.append(parameters.unpack())
    invocation.return_value(GLib.Variant("(u)", (len(notifications),)))


notifier = Gio.DBusNodeInfo.new_for_xml("""<node>
<interface name="org.freedesktop.Notifications">
<method name="Notify">
<arg type="s" direction="in"/><arg type="u" direction="in"/>
<arg type="s" direction="in"/><arg type="s" direction="in"/>
<arg type="s" direction="in"/><arg type="as" direction="in"/>
<arg type="a{sv}" direction="in"/><arg type="i" direction="in"/>
<arg type="u" direction="out"/>
</method></interface></node>""")
bus.register_object("/org/freedesktop/Notifications", notifier.interfaces[0], notified, None, None)
for name in ("org.kde.StatusNotifierWatcher", "org.freedesktop.Notifications"):
    bus.call_sync("org.freedesktop.DBus", "/org/freedesktop/DBus", "org.freedesktop.DBus",
                  "RequestName", GLib.Variant("(su)", (name, 0)), None, Gio.DBusCallFlags.NONE, 3000, None)

# GitHub: one run that is in progress until the run is marked finished.
finished = threading.Event()


def run():
    done = finished.is_set()
    return {
        "id": 7, "name": "CI", "display_title": "Add a page", "head_branch": "main",
        "event": "push", "status": "completed" if done else "in_progress",
        "conclusion": "success" if done else None,
        "html_url": "https://github.com/o/r/actions/runs/7",
        "run_started_at": "2026-11-16T13:30:42Z", "updated_at": "2026-11-16T13:34:54Z",
    }


class GitHub(http.server.BaseHTTPRequestHandler):
    def do_GET(self):
        check(self.headers["Authorization"] == "Bearer test-token", "the token is sent")
        if self.path.startswith("/repos/o/r/actions/runs/7/jobs"):
            body = {"jobs": [{"name": "check", "status": "in_progress", "conclusion": None,
                              "runner_name": "runberth-r"}]}
        elif self.path.startswith("/repos/o/r/actions/runs"):
            body = {"workflow_runs": [run()]}
        elif self.path.startswith("/user/repos"):
            body = [{"full_name": "o/r", "archived": False}, {"full_name": "o/new", "archived": False},
                    {"full_name": "o/old", "archived": True}]
        else:
            self.send_error(404)
            return
        data = json.dumps(body).encode()
        self.send_response(200)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(data)))
        self.end_headers()
        self.wfile.write(data)

    def log_message(self, *args):
        pass


api = http.server.ThreadingHTTPServer(("127.0.0.1", 0), GitHub)
threading.Thread(target=api.serve_forever, daemon=True).start()

home = Path(tempfile.mkdtemp())
settings = home / ".config/runberth/settings.json"
settings.parent.mkdir(parents=True)
settings.write_text(json.dumps({
    "server": {"host": "vps.example", "user": "ubuntu"},
    "github_token_command": "echo test-token",
    "interval_seconds": 5,
}))
status = {
    "format": 1, "version": "0.2.0",
    "memory": {"total_mib": 951, "available_mib": 615, "swap_total_mib": 3071, "swap_used_mib": 100},
    "mode": "on-demand", "dock": "active",
    "lock": [{"runner_id": "o-r", "repo": "o/r", "seconds": 30}],
    "runners": [{"repo": "o/r", "service": "active", "waiting": False}],
}
status_file = home / "status.json"
status_file.write_text(json.dumps(status))
connected = home / "connected"
ssh = home / "ssh"
# The status for `status --json`; `connect` records its arguments and the
# token it reads on stdin.
ssh.write_text(f"""#!/bin/sh
case "$*" in
  *"status --json") cat '{status_file}' ;;
  *" connect o/new") read token; echo "$token|$*" > '{connected}' ;;
  *) echo "runberth: unexpected $*" >&2; exit 1 ;;
esac
""")
ssh.chmod(0o755)

env = {
    **os.environ, "HOME": str(home), "XDG_CONFIG_HOME": str(home / ".config"),
    "RUNBERTH_APP_SSH": str(ssh), "RUNBERTH_APP_GITHUB_API": f"http://127.0.0.1:{api.server_port}",
}
app = subprocess.Popen([binary], env=env)
context = GLib.MainContext.default()


def wait(condition, seconds):
    deadline = time.monotonic() + seconds
    while time.monotonic() < deadline:
        while context.pending():
            context.iteration(False)
        if condition():
            return True
        time.sleep(0.05)
    return False


def call(destination, path, interface, method, parameters=None):
    return bus.call_sync(destination, path, interface, method, parameters, None,
                         Gio.DBusCallFlags.NONE, 3000, None).unpack()


def labels(node):
    _, props, children = node
    out = [props.get("label", "---" if props.get("type") == "separator" else "")]
    for child in children:
        out += ["  " + label for label in labels(child)]
    return out


def menu(owner):
    return call(owner, "/MenuBar", "com.canonical.dbusmenu", "GetLayout",
                GLib.Variant("(iias)", (0, -1, [])))[1]


def find(node, label):
    _, props, children = node
    if props.get("label") == label:
        return node
    for child in children:
        found = find(child, label)
        if found:
            return found
    return None


try:
    check(wait(lambda: registrations, 10), "the item registers with the tray host")
    owner = registrations[0][0] if registrations else None
    check(wait(lambda: owner and any("▶ r" in label for label in labels(menu(owner))), 15),
          "the menu shows the running job")
    if owner:
        shown = [label.strip() for label in labels(menu(owner))]
        print("\n".join(shown))
        check("ubuntu@vps.example · Runberth 0.2.0" in shown, "the server line")
        check("On demand · busy with r" in shown, "the lock holder")
        check("Edit Settings…" in shown, "settings open as a file on Linux")
        check(any("check — in progress on runberth-r" in label for label in shown), "the job and its runner")
        item = lambda name: call(owner, "/StatusNotifierItem", "org.freedesktop.DBus.Properties", "Get",
                                 GLib.Variant("(ss)", ("org.kde.StatusNotifierItem", name)))[0]
        running = item("IconName")
        check(running.startswith("runberth-tray-running-"), f"the running icon: {running}")
        check(wait(lambda: item("IconName") != running, 3), "the running icon turns")
        theme = Path(item("IconThemePath"))
        check((theme / "hicolor/scalable/apps/runberth-tray-running-0-symbolic.svg").is_file(),
              "the icon theme holds the tray icons")
        check(item("XAyatanaLabel") == "1", "the running count beside the icon")

        finished.set()
        status_file.write_text(json.dumps({**status, "lock": []}))
        check(wait(lambda: notifications, 20), "a notification when the run finishes")
        if notifications:
            app_name, _, _, summary, body, actions, hints, _ = notifications[0]
            check(summary == "✓ r: CI passed", f"notification title: {summary}")
            check(body == "main · Add a page · 4m", f"notification body: {body}")
            check(actions == ["default", "Open"], "the notification opens the run")
        check(wait(lambda: item("IconName") == "runberth-tray-success-symbolic", 10), "the passed icon")

        refresh = find(menu(owner), "Refresh Now")
        check(refresh is not None, "a Refresh Now item")
        if refresh:
            call(owner, "/MenuBar", "com.canonical.dbusmenu", "Event",
                 GLib.Variant("(isvu)", (refresh[0], "clicked", GLib.Variant("s", ""), 0)))
        time.sleep(1)
        check(len(notifications) == 1, "a finished run is announced once")

        connect = find(menu(owner), "Connect Repository")
        offered = [label.strip() for label in labels(connect)][1:] if connect else []
        check("o/new" in offered and "o/r" not in offered and "o/old" not in offered,
              f"unconnected, unarchived repositories are offered: {offered}")
        new = find(menu(owner), "o/new")
        if new:
            call(owner, "/MenuBar", "com.canonical.dbusmenu", "Event",
                 GLib.Variant("(isvu)", (new[0], "clicked", GLib.Variant("s", ""), 0)))
        check(wait(lambda: len(notifications) == 2, 10), "a notification when the repository connects")
        if len(notifications) == 2:
            check(notifications[1][3] == "Connected new", f"connect notification: {notifications[1][3]}")
        sent = connected.read_text().strip() if connected.exists() else ""
        token, _, command = sent.partition("|")
        check(token == "test-token", "the token reaches the server on stdin")
        check("test-token" not in command and command.endswith("/opt/runberth/bin/runberth connect o/new"),
              f"the server runs connect: {command}")
        # The same binary is the `runberth` command: a piped token wins.
        connected.unlink()
        cli = subprocess.run([binary, "connect", "o/new"], input="cli-token\n", text=True, env=env,
                             capture_output=True, timeout=20)
        check(cli.returncode == 0, f"runberth connect exits 0: {cli.stderr}")
        check(connected.exists() and connected.read_text().startswith("cli-token|"),
              "runberth connect passes a piped token on stdin")
        usage = subprocess.run([binary, "connect"], env=env, capture_output=True, text=True, timeout=20)
        check(usage.returncode == 2 and "Usage: runberth" in usage.stderr, "a mistake prints the usage")
    app.send_signal(signal.SIGTERM)
    check(app.wait(10) == 0, "SIGTERM quits cleanly")
finally:
    if app.poll() is None:
        app.kill()
    api.shutdown()

print("FAILED" if failures else "OK", flush=True)
sys.exit(1 if failures else 0)
