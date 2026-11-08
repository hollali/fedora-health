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
        self.set_default_size(980, 700)

        toolbar = Adw.ToolbarView()
        header = Adw.HeaderBar()
        toolbar.add_top_bar(header)

        self.stack = Adw.ViewStack()
        switcher = Adw.ViewSwitcher()
        switcher.set_stack(self.stack)
        header.set_title_widget(switcher)

        # ---------- Overview ----------
        overview_page = Adw.PreferencesPage()
        boot_group = Adw.PreferencesGroup(title="Startup")
        self.boot_row = Adw.ActionRow(title="Boot time", subtitle="Unknown")
        boot_group.add(self.boot_row)
        overview_page.add(boot_group)

        diag_action_group = Adw.PreferencesGroup(title="Diagnosis")
        check_row = Adw.ActionRow(title="Check system", subtitle="Run all health checks")
        btn_check = Gtk.Button(label="Check now")
        btn_check.set_use_underline("_Check now")
        btn_check.add_css_class("suggested-action")
        btn_check.connect("clicked", lambda *_: self.run_diagnosis())
        btn_check.set_tooltip_text("Run a full system health check")
        check_row.add_suffix(btn_check)
        diag_action_group.add(check_row)
        self.status_row = Adw.ActionRow(title="Status", subtitle="Ready")
        diag_action_group.add(self.status_row)
        overview_page.add(diag_action_group)
        self.stack.add_titled(overview_page, "overview", "Overview")

        # ---------- Diagnosis ----------
        diagnosis_page = Adw.PreferencesPage()
        self.diag_group = Adw.PreferencesGroup(title="Findings")
        diagnosis_page.add(self.diag_group)
        self.stack.add_titled(diagnosis_page, "diagnosis", "Diagnosis")

        # ---------- Startup ----------
        startup_page = Adw.PreferencesPage()
        startup_group = Adw.PreferencesGroup(title="Speed up startup")
        speed_row = Adw.ActionRow(title="Move slow services out of boot",
                                 subtitle="Defers heavy services and socket-activates Docker/MariaDB")
        btn_speed = Gtk.Button(label="Apply")
        btn_speed.set_use_underline("_Apply")
        btn_speed.add_css_class("suggested-action")
        btn_speed.connect("clicked", lambda *_: self.run_fix("services-slowboot"))
        btn_speed.set_tooltip_text("Apply changes to move slow services out of boot")
        speed_row.add_suffix(btn_speed)
        startup_group.add(speed_row)

        io_row = Adw.ActionRow(title="Disk settings",
                               subtitle="Use BFQ scheduler on rotational disks")
        btn_io = Gtk.Button(label="Apply")
        btn_io.set_use_underline("_Apply")
        btn_io.connect("clicked", lambda *_: self.run_fix("io-scheduler"))
        btn_io.set_tooltip_text("Apply disk I/O scheduler settings")
        io_row.add_suffix(btn_io)
        startup_group.add(io_row)
        startup_page.add(startup_group)
        self.stack.add_titled(startup_page, "startup", "Startup")

        # ---------- Services ----------
        services_page = Adw.PreferencesPage()
        services_top = Adw.PreferencesGroup(title="Boot services")
        refresh_row = Adw.ActionRow(title="Refresh list", subtitle="Top services by boot time")
        btn_refresh = Gtk.Button(label="Refresh")
        btn_refresh.connect("clicked", lambda *_: self.load_services())
        refresh_row.add_suffix(btn_refresh)
        services_top.add(refresh_row)
        services_page.add(services_top)
        self.services_group = Adw.PreferencesGroup(title="Services")
        services_page.add(self.services_group)
        self.stack.add_titled(services_page, "services", "Services")

        # ---------- Actions ----------
        actions_page = Adw.PreferencesPage()
        fixes_group = Adw.PreferencesGroup(title="Maintenance")
        fixall_row = Adw.ActionRow(title="Fix all safe issues", subtitle="Runs every applicable safe fix")
        btn_fixall = Gtk.Button(label="Fix all")
        btn_fixall.set_use_underline("_Fix all")
        btn_fixall.add_css_class("suggested-action")
        btn_fixall.connect("clicked", lambda *_: self.run_fix(None))
        btn_fixall.set_tooltip_text("Apply all safe fixes")
        fixall_row.add_suffix(btn_fixall)
        fixes_group.add(fixall_row)

        reboot_row = Adw.ActionRow(title="Restart computer", subtitle="Reboot to apply boot changes")
        btn_reboot = Gtk.Button(label="Restart")
        btn_reboot.set_use_underline("_Restart")
        btn_reboot.add_css_class("destructive-action")
        btn_reboot.connect("clicked", lambda *_: self.reboot())
        btn_reboot.set_tooltip_text("Restart your computer to apply boot changes")
        reboot_row.add_suffix(btn_reboot)
        fixes_group.add(reboot_row)
        actions_page.add(fixes_group)
        self.stack.add_titled(actions_page, "actions", "Actions")

        toolbar.set_content(self.stack)
        self.set_content(toolbar)

        self.findings = []
        self.diag_rows = []
        self.service_rows = []
        GLib.idle_add(self.refresh_boot)
        GLib.idle_add(self.load_services)

    def set_status(self, text):
        self.status_row.set_subtitle(text)

    def refresh_boot(self):
        try:
            out = subprocess.run(["systemd-analyze"], capture_output=True, text=True, timeout=10)
            text = out.stdout.strip() or "Unknown"
            self.boot_row.set_subtitle(text)
        except Exception as e:
            self.boot_row.set_subtitle(str(e))
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
        for row in self.diag_rows:
            self.diag_group.remove(row)
        self.diag_rows = []
        for item in data:
            row = self._make_finding_row(item)
            self.diag_group.add(row)
            self.diag_rows.append(row)
        self.set_status(f"Found {len(data)} items.")
        return False

    def _severity_dot(self, sev):
        lbl = Gtk.Label(label="●")
        css = {"ok": "success", "warn": "warning", "bad": "error"}.get(sev, "")
        if css:
            lbl.add_css_class(css)
        return lbl

    def _make_finding_row(self, item):
        sev = item.get("severity", "ok")
        title = item.get("title", "")
        detail = item.get("detail", "")
        row = Adw.ActionRow(title=title, subtitle=detail)
        row.set_accessible_role(Gtk.AccessibleRole.LIST_ITEM)
        row.add_prefix(self._severity_dot(sev))
        fid = item.get("id")
        if item.get("fix") == 1 and fid:
            btn = Gtk.Button(label="Fix")
            btn.add_css_class("suggested-action")
            btn.connect("clicked", lambda *_: self.run_fix(fid))
            row.add_suffix(btn)
        return row

    def run_fix(self, fid):
        def worker():
            try:
                if fid is None:
                    cmd = ["pkexec", DOCTOR, "--fix"]
                else:
                    cmd = ["pkexec", DOCTOR, "--fix-id", fid]
                self.set_status("Applying fix… (password may be required)")
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
                items = []
                for ln in out.stdout.splitlines():
                    ln = ln.strip()
                    if not ln:
                        continue
                    parts = ln.split()
                    unit = parts[-1]
                    if not unit.endswith(".service"):
                        continue
                    items.append((ln, unit))
                    if len(items) >= 25:
                        break
                GLib.idle_add(self._show_services, items)
            except Exception as e:
                GLib.idle_add(self.set_status, f"Services load failed: {e}")
        threading.Thread(target=worker, daemon=True).start()

    def _show_services(self, items):
        for row in self.service_rows:
            self.services_group.remove(row)
        self.service_rows = []
        for text, unit in items:
            row = self._make_service_row(text, unit)
            self.services_group.add(row)
            self.service_rows.append(row)
        return False

    def _make_service_row(self, text, unit):
        row = Adw.ActionRow(title=unit, subtitle=text)
        row.set_accessible_role(Gtk.AccessibleRole.LIST_ITEM)
        state = subprocess.run(["systemctl", "is-enabled", unit], capture_output=True, text=True).stdout.strip()
        if state in ("enabled", "enabled-runtime"):
            btn = Gtk.Button(label="Disable")
            btn.connect("clicked", lambda *_, u=unit: self.manage_service(u, "disable"))
        else:
            btn = Gtk.Button(label="Enable")
            btn.connect("clicked", lambda *_, u=unit: self.manage_service(u, "enable"))
        row.add_suffix(btn)

        if state == "masked":
            btn2 = Gtk.Button(label="Unmask")
            btn2.connect("clicked", lambda *_, u=unit: self.manage_service(u, "unmask"))
        else:
            btn2 = Gtk.Button(label="Mask")
            btn2.connect("clicked", lambda *_, u=unit: self.manage_service(u, "mask"))
        row.add_suffix(btn2)
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