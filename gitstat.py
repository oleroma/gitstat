#!/usr/bin/env python3
"""gitstat: a tray icon showing whether the git repos in one folder are in sync.

Every repo directly under the configured folder is fetched on a timer (fetch
only, never pull) and compared against its upstream. The icon is green with a
checkmark when everything is current, red with a count otherwise. The menu
lists each repo with its status.

Usage:
    python gitstat.py              run the tray app
    python gitstat.py --once       print statuses once and exit (no tray)
    python gitstat.py --config F   use config file F (default: config.json
                                   next to this script)
"""

import argparse
import json
import os
import subprocess
import sys
import threading
from dataclasses import dataclass
from pathlib import Path

DEFAULT_CONFIG = {"path": "~/dev", "interval_minutes": 5}
FETCH_TIMEOUT = 60

# Hide console windows spawned by git on Windows when run under pythonw.
_POPEN_FLAGS = getattr(subprocess, "CREATE_NO_WINDOW", 0)


@dataclass
class RepoStatus:
    name: str
    ahead: int = 0
    behind: int = 0
    dirty: bool = False
    upstream: bool = True
    error: str = ""

    @property
    def synced(self):
        return (self.upstream and not self.error and not self.dirty
                and self.ahead == 0 and self.behind == 0)

    @property
    def label(self):
        if self.error:
            return f"error: {self.error}"
        parts = []
        if not self.upstream:
            parts.append("no upstream")
        if self.ahead:
            parts.append(f"⬆{self.ahead}")
        if self.behind:
            parts.append(f"⬇{self.behind}")
        if self.ahead and self.behind:
            parts.append("diverged")
        if self.dirty:
            parts.append("dirty")
        return " ".join(parts) or "✓ current"


def load_config(path):
    config = dict(DEFAULT_CONFIG)
    if path.exists():
        with open(path, encoding="utf-8") as f:
            config.update(json.load(f))
    config["path"] = Path(os.path.expandvars(config["path"])).expanduser()
    config["interval_minutes"] = max(1, float(config["interval_minutes"]))
    return config


def git(repo, *args, timeout=30):
    return subprocess.run(
        ["git", "-C", str(repo), *args],
        capture_output=True, text=True, timeout=timeout,
        stdin=subprocess.DEVNULL, creationflags=_POPEN_FLAGS,
        env={**os.environ, "GIT_TERMINAL_PROMPT": "0"},
    )


def find_repos(parent):
    if not parent.is_dir():
        return []
    return sorted((p for p in parent.iterdir() if (p / ".git").exists()),
                  key=lambda p: p.name.lower())


def check_repo(repo, fetch=True):
    status = RepoStatus(repo.name)
    try:
        if fetch:
            result = git(repo, "fetch", "--quiet", "--all", "--prune",
                         timeout=FETCH_TIMEOUT)
            if result.returncode != 0:
                lines = result.stderr.strip().splitlines()
                status.error = "fetch failed" + (f" ({lines[-1]})" if lines else "")
                return status

        dirty = git(repo, "status", "--porcelain")
        if dirty.returncode != 0:
            status.error = "not readable"
            return status
        status.dirty = bool(dirty.stdout.strip())

        counts = git(repo, "rev-list", "--left-right", "--count", "HEAD...@{upstream}")
        if counts.returncode != 0:
            status.upstream = False
        else:
            ahead, behind = counts.stdout.split()
            status.ahead, status.behind = int(ahead), int(behind)
    except subprocess.TimeoutExpired:
        status.error = "timed out"
    except OSError as e:
        status.error = str(e)
    return status


def check_all(parent, fetch=True):
    return [check_repo(r, fetch) for r in find_repos(parent)]


def make_icon(unsynced, size=64):
    from PIL import Image, ImageDraw, ImageFont

    img = Image.new("RGBA", (size, size), (0, 0, 0, 0))
    draw = ImageDraw.Draw(img)
    color = (46, 160, 67) if unsynced == 0 else (215, 58, 73)
    draw.ellipse((2, 2, size - 2, size - 2), fill=color)
    if unsynced == 0:
        s = size / 64
        draw.line([(18 * s, 33 * s), (28 * s, 44 * s), (47 * s, 22 * s)],
                  fill="white", width=int(7 * s), joint="curve")
    else:
        text = str(unsynced) if unsynced < 100 else "99+"
        font_size = int(size * (0.6 if len(text) == 1 else 0.48 if len(text) == 2 else 0.36))
        try:
            font = ImageFont.load_default(size=font_size)
        except TypeError:  # Pillow < 10.1
            font = ImageFont.load_default()
        draw.text((size / 2, size / 2), text, fill="white", font=font, anchor="mm")
    return img


class TrayApp:
    def __init__(self, config):
        import pystray

        self.pystray = pystray
        self.config = config
        self.statuses = []
        self.checking = False
        self.lock = threading.Lock()
        self.wake = threading.Event()
        self.stopped = threading.Event()
        self.icon = pystray.Icon("gitstat", make_icon(0), "gitstat", menu=self.build_menu())

    def build_menu(self):
        Item, Menu = self.pystray.MenuItem, self.pystray.Menu
        items = []
        if self.checking and not self.statuses:
            items.append(Item("Checking…", None, enabled=False))
        elif not self.statuses:
            items.append(Item(f"No repos in {self.config['path']}", None, enabled=False))
        for s in self.statuses:
            items.append(Item(f"{s.name}: {s.label}", self.open_repo(s.name)))
        items += [
            Menu.SEPARATOR,
            Item("Checking…" if self.checking else "Refresh now",
                 lambda: self.wake.set(), enabled=not self.checking),
            Item("Quit", self.quit),
        ]
        return Menu(*items)

    def open_repo(self, name):
        path = self.config["path"] / name

        def action():
            if sys.platform == "win32":
                os.startfile(path)
            else:
                opener = "open" if sys.platform == "darwin" else "xdg-open"
                subprocess.Popen([opener, str(path)])
        return action

    def refresh(self):
        with self.lock:
            self.checking = True
            self.update_icon()
            try:
                self.statuses = check_all(self.config["path"])
            finally:
                self.checking = False
                self.update_icon()

    def update_icon(self):
        unsynced = sum(not s.synced for s in self.statuses)
        self.icon.icon = make_icon(unsynced)
        if self.checking:
            self.icon.title = "gitstat: checking…"
        elif unsynced:
            self.icon.title = f"gitstat: {unsynced} of {len(self.statuses)} repos unsynced"
        else:
            self.icon.title = f"gitstat: all {len(self.statuses)} repos current"
        self.icon.menu = self.build_menu()  # setter also refreshes the native menu

    def loop(self):
        while not self.stopped.is_set():
            self.refresh()
            self.wake.wait(self.config["interval_minutes"] * 60)
            self.wake.clear()

    def quit(self):
        self.stopped.set()
        self.wake.set()
        self.icon.stop()

    def run(self):
        def setup(icon):
            icon.visible = True
            threading.Thread(target=self.loop, daemon=True).start()
        self.icon.run(setup=setup)


def main():
    parser = argparse.ArgumentParser(description="Tray icon for git repo sync status.")
    parser.add_argument("--config", type=Path,
                        default=Path(__file__).resolve().with_name("config.json"))
    parser.add_argument("--once", action="store_true",
                        help="print statuses once and exit")
    parser.add_argument("--no-fetch", action="store_true",
                        help="with --once, skip git fetch")
    args = parser.parse_args()
    config = load_config(args.config)

    if args.once:
        statuses = check_all(config["path"], fetch=not args.no_fetch)
        if not statuses:
            print(f"No git repos found in {config['path']}")
        width = max((len(s.name) for s in statuses), default=0)
        for s in statuses:
            print(f"{s.name:<{width}}  {s.label}")
        sys.exit(0 if all(s.synced for s in statuses) else 1)

    TrayApp(config).run()


if __name__ == "__main__":
    main()
