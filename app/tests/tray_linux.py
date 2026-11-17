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
ssh = home / "ssh"
ssh.write_text(f"#!/bin/sh\ncat '{status_file}'\n")
ssh.chmod(0o755)

app = subprocess.Popen([binary], env={
    **os.environ, "HOME": str(home), "XDG_CONFIG_HOME": str(home / ".config"),
    "RUNBERTH_APP_SSH": str(ssh), "RUNBERTH_APP_GITHUB_API": f"http://127.0.0.1:{api.server_port}",
})
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
        check(item("IconName") == "runberth-tray-running-symbolic", "the running icon")
        theme = Path(item("IconThemePath"))
        check((theme / "hicolor/scalable/apps/runberth-tray-running-symbolic.svg").is_file(),
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
        check(wait(lambda: item("IconName") == "runberth-tray-symbolic", 10), "the idle icon")

        refresh = find(menu(owner), "Refresh Now")
        check(refresh is not None, "a Refresh Now item")
        if refresh:
            call(owner, "/MenuBar", "com.canonical.dbusmenu", "Event",
                 GLib.Variant("(isvu)", (refresh[0], "clicked", GLib.Variant("s", ""), 0)))
        time.sleep(1)
        check(len(notifications) == 1, "a finished run is announced once")
    app.send_signal(signal.SIGTERM)
    check(app.wait(10) == 0, "SIGTERM quits cleanly")
finally:
    if app.poll() is None:
        app.kill()
    api.shutdown()

print("FAILED" if failures else "OK", flush=True)
sys.exit(1 if failures else 0)
