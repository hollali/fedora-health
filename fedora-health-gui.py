#!/usr/bin/env python3
"""Simple GUI for fedora-health (non-technical users)."""
import json
import os
import subprocess
import threading

import gi
gi.require_version("Gtk", "4.0")
gi.require_version("Adw", "1")
from gi.repository import Adw, GLib, Gtk


def doctor_path():
    for p in ("/usr/local/bin/fedora-doctor.sh", "/usr/bin/fedora-doctor.sh"):
        if os.path.exists(p):
            return p
    here = os.path.dirname(os.path.abspath(__file__))
    cand = os.path.join(here, "fedora-doctor.sh")
    if os.path.exists(cand):
        return cand
    return "fedora-doctor.sh"


DOCTOR = doctor_path()


class FedoraHealthWindow(Adw.ApplicationWindow):
    def __init__(self, app):
        super().__init__(application=app, title="Fedora Health")
        self.set_default_size(920, 640)

        toolbar = Adw.ToolbarView()
        header = Adw.HeaderBar()
        toolbar.add_top_bar(header)

        notebook = Gtk.Notebook()
        toolbar.set_content(notebook)

        # Overview
        overview = Gtk.Box(orientation=Gtk.Orientation.VERTICAL, spacing=12)
        overview.set_margin_top(16)
        overview.set_margin_bottom(16)
        overview.set_margin_start(16)
        overview.set_margin_end(16)
        self.boot_label = Gtk.Label(label="Boot time: unknown")
        self.boot_label.set_wrap(True)
        overview.append(self.boot_label)
        btn_check = Gtk.Button(label="Check system now")
        btn_check.connect("clicked", lambda *_: self.run_diagnosis())
        overview.append(btn_check)
        self.status_label = Gtk.Label(label="Ready.")
        self.status_label.set_wrap(True)
        overview.append(self.status_label)
        notebook.append_page(overview, Gtk.Label(label="Overview"))

        # Diagnosis
        diag_box = Gtk.Box(orientation=Gtk.Orientation.VERTICAL, spacing=8)
        diag_box.set_margin_top(8)
        diag_box.set_margin_bottom(8)
        diag_box.set_margin_start(8)
        diag_box.set_margin_end(8)
        self.diag_list = Gtk.ListBox()
        self.diag_list.set_selection_mode(Gtk.SelectionMode.NONE)
        scroll = Gtk.ScrolledWindow()
        scroll.set_child(self.diag_list)
        scroll.set_vexpand(True)
        diag_box.append(scroll)
        notebook.append_page(diag_box, Gtk.Label(label="Diagnosis"))

        # Startup
        startup = Gtk.Box(orientation=Gtk.Orientation.VERTICAL, spacing=12)
        startup.set_margin_top(16)
        startup.set_margin_bottom(16)
        startup.set_margin_start(16)
        startup.set_margin_end(16)
        self.startup_label = Gtk.Label(label="Make startup faster by moving slow services out of boot.")
        self.startup_label.set_wrap(True)
        startup.append(self.startup_label)
        btn_speed = Gtk.Button(label="Speed up startup (safe)")
        btn_speed.connect("clicked", lambda *_: self.run_fix("services-slowboot"))
        startup.append(btn_speed)
        btn_io = Gtk.Button(label="Use better disk settings")
        btn_io.connect("clicked", lambda *_: self.run_fix("io-scheduler"))
        startup.append(btn_io)
        notebook.append_page(startup, Gtk.Label(label="Startup"))

        # Services
        services = Gtk.Box(orientation=Gtk.Orientation.VERTICAL, spacing=8)
        services.set_margin_top(8)
        services.set_margin_bottom(8)
        services.set_margin_start(8)
        services.set_margin_end(8)
        btn_refresh_services = Gtk.Button(label="Refresh services")
        btn_refresh_services.connect("clicked", lambda *_: self.load_services())
        services.append(btn_refresh_services)
        self.services_list = Gtk.ListBox()
        self.services_list.set_selection_mode(Gtk.SelectionMode.NONE)
        scroll_services = Gtk.ScrolledWindow()
        scroll_services.set_child(self.services_list)
        scroll_services.set_vexpand(True)
        services.append(scroll_services)
        notebook.append_page(services, Gtk.Label(label="Services"))

        # Actions
        actions = Gtk.Box(orientation=Gtk.Orientation.VERTICAL, spacing=12)
        actions.set_margin_top(16)
        actions.set_margin_bottom(16)
        actions.set_margin_start(16)
        actions.set_margin_end(16)
        btn_fixall = Gtk.Button(label="Fix all safe issues")
        btn_fixall.connect("clicked", lambda *_: self.run_fix(None))
        actions.append(btn_fixall)
        btn_reboot = Gtk.Button(label="Restart computer")
        btn_reboot.connect("clicked", lambda *_: self.reboot())
        actions.append(btn_reboot)
        notebook.append_page(actions, Gtk.Label(label="Actions"))

        self.set_content(toolbar)

        self.findings = []
        GLib.idle_add(self.refresh_boot)
        GLib.idle_add(self.load_services)

    def set_status(self, text):
        self.status_label.set_label(text)

    def refresh_boot(self):
        try:
            out = subprocess.run(["systemd-analyze"], capture_output=True, text=True, timeout=10)
            self.boot_label.set_label(out.stdout.strip() or "Boot time: unknown")
        except Exception as e:
            self.boot_label.set_label(f"Boot time: {e}")
        return False

    def run_diagnosis(self):
        self.set_status("Checking system…")
        threading.Thread(target=self._diagnosis_worker, daemon=True).start()

    def _diagnosis_worker(self):
        try:
            out = subprocess.run([DOCTOR, "--json"], capture_output=True, text=True, timeout=120)
            data = json.loads(out.stdout)
            GLib.idle_add(self._show_findings, data)
        except Exception as e:
            GLib.idle_add(self.set_status, f"Diagnosis failed: {e}")

    def _show_findings(self, data):
        # clear
        while True:
            row = self.diag_list.get_row_at_index(0)
            if row is None:
                break
            self.diag_list.remove(row)
        for item in data:
            row = self._make_row(item)
            self.diag_list.append(row)
        self.set_status(f"Found {len(data)} items.")
        return False

    def _make_row(self, item):
        box = Gtk.Box(orientation=Gtk.Orientation.HORIZONTAL, spacing=8)
        box.set_margin_top(6)
        box.set_margin_bottom(6)
        box.set_margin_start(6)
        box.set_margin_end(6)

        sev = item.get("severity", "ok")
        color = {"ok": "green", "warn": "orange", "bad": "red"}.get(sev, "gray")
        lbl = Gtk.Label()
        lbl.set_markup(f"<span foreground='{color}'><b>[{sev.upper()}]</b></span> {GLib.markup_escape_text(item.get('title',''))}")
        lbl.set_halign(Gtk.Align.START)
        lbl.set_wrap(True)
        lbl.set_hexpand(True)
        box.append(lbl)

        fid = item.get("id")
        if item.get("fix") == 1 and fid:
            btn = Gtk.Button(label="Fix")
            btn.connect("clicked", lambda *_: self.run_fix(fid))
            box.append(btn)

        row = Gtk.ListBoxRow()
        row.set_child(box)
        # store detail as tooltip
        row.set_tooltip_text(item.get("detail", ""))
        return row

    def run_fix(self, fid):
        def worker():
            try:
                if fid is None:
                    cmd = ["pkexec", DOCTOR, "--fix"]
                else:
                    cmd = ["pkexec", DOCTOR, "--fix-id", fid]
                self.set_status("Applying fix… (you may be asked for your password)")
                subprocess.run(cmd, check=False, timeout=300)
                GLib.idle_add(self.set_status, "Fix finished. Re-checking…")
                GLib.idle_add(self.run_diagnosis)
            except Exception as e:
                GLib.idle_add(self.set_status, f"Fix failed: {e}")
        threading.Thread(target=worker, daemon=True).start()

    def load_services(self):
        def worker():
            try:
                out = subprocess.run(["systemd-analyze", "blame"], capture_output=True, text=True, timeout=60)
                lines = [ln.strip() for ln in out.stdout.splitlines() if ln.strip()]
                items = []
                for ln in lines[:25]:
                    parts = ln.split()
                    if not parts:
                        continue
                    unit = parts[-1]
                    if not unit.endswith(".service"):
                        continue
                    items.append((ln, unit))
                GLib.idle_add(self._show_services, items)
            except Exception as e:
                GLib.idle_add(self.set_status, f"Services load failed: {e}")
        threading.Thread(target=worker, daemon=True).start()

    def _show_services(self, items):
        while True:
            row = self.services_list.get_row_at_index(0)
            if row is None:
                break
            self.services_list.remove(row)
        for text, unit in items:
            self.services_list.append(self._make_service_row(text, unit))
        return False

    def _make_service_row(self, text, unit):
        box = Gtk.Box(orientation=Gtk.Orientation.HORIZONTAL, spacing=8)
        box.set_margin_top(4)
        box.set_margin_bottom(4)
        box.set_margin_start(4)
        box.set_margin_end(4)
        lbl = Gtk.Label(label=text)
        lbl.set_halign(Gtk.Align.START)
        lbl.set_hexpand(True)
        box.append(lbl)

        state = subprocess.run(["systemctl", "is-enabled", unit], capture_output=True, text=True).stdout.strip()
        if state in ("enabled", "enabled-runtime"):
            b = Gtk.Button(label="Disable")
            b.connect("clicked", lambda *_, u=unit: self.manage_service(u, "disable"))
            box.append(b)
        else:
            b = Gtk.Button(label="Enable")
            b.connect("clicked", lambda *_, u=unit: self.manage_service(u, "enable"))
            box.append(b)

        if state == "masked":
            b2 = Gtk.Button(label="Unmask")
            b2.connect("clicked", lambda *_, u=unit: self.manage_service(u, "unmask"))
        else:
            b2 = Gtk.Button(label="Mask")
            b2.connect("clicked", lambda *_, u=unit: self.manage_service(u, "mask"))
        box.append(b2)

        row = Gtk.ListBoxRow()
        row.set_child(box)
        return row

    def manage_service(self, unit, action):
        def worker():
            try:
                self.set_status(f"{action} {unit}…")
                subprocess.run(["pkexec", "systemctl", action, unit], check=False, timeout=120)
                GLib.idle_add(self.load_services)
                GLib.idle_add(self.set_status, f"{action} {unit} done.")
            except Exception as e:
                GLib.idle_add(self.set_status, f"{action} failed: {e}")
        threading.Thread(target=worker, daemon=True).start()

    def reboot(self):
        subprocess.run(["systemctl", "reboot"], check=False)


class FedoraHealthApp(Adw.Application):
    def __init__(self):
        super().__init__(application_id="org.fedoraproject.FedoraHealth")

    def do_activate(self):
        win = FedoraHealthWindow(self)
        win.present()


def main():
    app = FedoraHealthApp()
    app.run()


if __name__ == "__main__":
    main()
