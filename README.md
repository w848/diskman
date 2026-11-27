# diskman

See what takes the space on your drives, and free it.

diskman is a terminal app for Windows. It lists your drives and scans a drive or folder on all CPU cores. You can then browse the folder tree biggest first and send what you don't need to the Recycle Bin, or delete it. It also has a cleanup screen for the Windows Update files that are safe to delete.

## Build and run

You need [Rust](https://rustup.rs) 1.88 or newer.

```sh
cargo build --release
```

The program is built as `target\release\diskman.exe`. Run it like this:

```sh
diskman            # start on the list of drives
diskman D:\Games   # scan that folder right away
```

Run it as administrator to scan protected system folders and to use the Windows Update cleanup.

## Keys

| Key | What it does |
| --- | --- |
| `↑` `↓` or `j` `k` | Move |
| `PgUp` `PgDn` `Home` `End` | Move faster |
| `Enter`, `→` or `l` | Open the folder, or scan the drive |
| `Backspace`, `←` or `h` | Back to the folder above |
| `d` or `Delete` | Move to the Recycle Bin |
| `D` or `Shift+Delete` | Delete permanently |
| `e` | Show in Explorer |
| `s` | Sort by size or by name |
| `r` | Scan this folder again |
| `w` | Windows Update cleanup |
| `p` | Programs, least recently used first |
| `Esc` | Stop a scan, or go back |
| `?` | Help |
| `q` or `Ctrl+C` | Quit |

You can also use the mouse. Click to select, double-click to open, right-click to go back, and scroll with the wheel.

Every delete asks you to confirm first. After a delete, the folder sizes update without a rescan.

## Windows Update cleanup

Press `w` to open the cleanup screen. It lists the Windows Update files that are safe to delete, with their sizes. `Space` ticks or unticks a line, `a` ticks all the safe ones, and `D` deletes everything ticked after you confirm.

| What | Ticked when the screen opens | What diskman does |
| --- | --- | --- |
| Downloaded updates (`Windows\SoftwareDistribution\Download`) | Yes | Empties it. Windows downloads updates again if it needs them. |
| Delivery Optimization cache | Yes | Empties it. |
| Upgrade leftovers (`$Windows.~BT`, `$Windows.~WS`) | Yes | Deletes them. |
| Old versions of updated components (`WinSxS`) | Yes | Runs `DISM /Online /Cleanup-Image /StartComponentCleanup`. This can take 5 to 30 minutes. |
| Previous Windows (`Windows.old`) | No | Deletes it. After that, you can no longer go back to the previous Windows. |

Things to know:

- **Administrator rights:** you need them to delete. Without them, the screen only shows the sizes.
- **Deleted permanently:** these files don't go to the Recycle Bin.
- **Files in use:** files that Windows is using are skipped, and you can delete them after a restart.
- **WinSxS:** its size isn't shown. Most of its files are shared with Windows itself, so adding them up would show far more than it really takes. diskman reports what DISM freed from how much free space the drive gains.
- **No `/ResetBase`:** DISM runs without it, so you can still uninstall updates you've already installed.
- **Never touched:** the rest of `WinSxS` (except through DISM), `Windows\Installer` and `SoftwareDistribution\DataStore`. Windows needs them, and DataStore holds your update history.

## Programs you don't use

Press `p` to see your installed programs, the ones used longest ago first, with their sizes. Programs not used in 90 days or more are shown in yellow.

| Key | What it does |
| --- | --- |
| `Enter` or `u` | Run the program's own uninstaller, after you confirm |
| `x` | Save the list as a CSV file in Documents, with each uninstall command |
| `e` | Open the program's folder |
| `s` | Sort by last use, by size or by name |
| `r` | Read the list again, after an uninstall |

How diskman knows when a program was last used:

- **Launch history:** Windows records the programs you start from the Start menu, the taskbar or Explorer. diskman can always read this.
- **Prefetch:** when diskman runs as administrator, it also reads Prefetch, which notes every program run lately. This includes games started from a launcher and tools run from a terminal.

"No record" means neither of these saw the program start. It doesn't prove the program is never used, so run diskman as administrator for the best picture.

Store apps don't appear in the list, because Windows keeps them elsewhere.

## Tests

```sh
cargo test
```

## License

MIT. See [LICENSE](LICENSE).
