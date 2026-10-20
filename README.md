# gitstat

A tray / menu-bar icon that watches every git repo directly under one folder
and shows at a glance whether they're in sync with their remotes.

- **Green ✓**: every repo is current.
- **Red with a number**: that many repos are unsynced.
- **Right-click the icon** (click it on macOS) for the list: each repo with its status (`⬆2`, `⬇1`,
  `diverged`, `dirty`, `no upstream`, `✓ current`), plus **Refresh now** and
  **Quit**. Clicking a repo opens its folder.

Every `interval_minutes` it runs `git fetch` in each repo. It only fetches; it
never pulls, merges or pushes. A repo counts as unsynced if it is ahead,
behind, diverged, has uncommitted changes, has no upstream branch, or its fetch
failed.

## Setup

You need Python 3.8+ and git on your `PATH`.

```sh
git clone https://github.com/oleroma/gitstat.git
cd gitstat
python3 -m pip install -r requirements.txt
```

Edit `config.json`:

```json
{
  "path": "~/dev",
  "interval_minutes": 5
}
```

`path` is the parent folder whose direct subfolders are your repos (`~` and
environment variables like `%USERPROFILE%` are expanded). On Windows use
forward slashes or doubled backslashes, e.g. `"C:/Users/me/dev"`.

Check it without the tray first. This prints every repo's status and exits:

```sh
python3 gitstat.py --once
```

Then run the tray app:

```sh
python3 gitstat.py
```

Pass `--config other.json` to use a config file somewhere else.

Fetches run non-interactively, so repos whose remotes need a password prompt
will show `fetch failed`. Use an SSH agent or a credential helper for those.

## Start on login

Replace `/path/to/gitstat` with where you cloned it.

### Windows

Run this once in PowerShell from the gitstat folder. It puts a shortcut in
your Startup folder that launches gitstat with `pythonw`, which runs it
without a console window:

```powershell
$pyw = (Get-Command pythonw).Source
$lnk = (New-Object -ComObject WScript.Shell).CreateShortcut("$([Environment]::GetFolderPath('Startup'))\gitstat.lnk")
$lnk.TargetPath = $pyw
$lnk.Arguments = "`"$PWD\gitstat.py`""
$lnk.WorkingDirectory = "$PWD"
$lnk.Save()
```

To start it right away, double-click the new shortcut (`Win+R`,
`shell:startup`). To stop it starting on login, delete that shortcut. If
the icon lands in the hidden-icons overflow, drag it onto the taskbar.

### macOS

Save this as `~/Library/LaunchAgents/com.gitstat.plist`, fixing the two paths
(`which python3` tells you the first one):

```xml
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>Label</key><string>com.gitstat</string>
  <key>ProgramArguments</key>
  <array>
    <string>/usr/local/bin/python3</string>
    <string>/path/to/gitstat/gitstat.py</string>
  </array>
  <key>RunAtLoad</key><true/>
</dict>
</plist>
```

Load it now (it then starts on every login):

```sh
launchctl load ~/Library/LaunchAgents/com.gitstat.plist
```

### Linux

Save this as `~/.config/autostart/gitstat.desktop`:

```ini
[Desktop Entry]
Type=Application
Name=gitstat
Exec=python3 /path/to/gitstat/gitstat.py
X-GNOME-Autostart-enabled=true
```

KDE, XFCE, Cinnamon and most other desktops show tray icons out of the box.
On GNOME, install the "AppIndicator and KStatusNotifierItem Support"
extension. If the icon doesn't appear, install the AppIndicator bindings
(`sudo apt install gir1.2-ayatanaappindicator3-0.1 python3-gi` on
Debian/Ubuntu) and use the system Python, or create your venv with
`--system-site-packages`.
