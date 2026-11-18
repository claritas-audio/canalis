# ADR 0006: the service owns the audio, windows are clients over a Unix socket

Date: 2026-11-12. Status: accepted.

## Context
Up to now the window ran its own control plane and stopped the systemd service while it was open, so every chain restarted when a window opened or closed. There was no way for a CLI call (`canalis scene <name>`) or a tray icon to reach a running control plane, and two processes took turns writing `config.toml`.

## Decision
- **One process owns the audio:** `canalis --daemon`, the service. Windows, the tray and CLI calls are its clients. A window starts the service when none answers and never stops it. `canalis quit` and the tray's Quit stop it; it then says `bye` and windows close.
- **Transport: a Unix socket with JSON lines**, at `$XDG_RUNTIME_DIR/canalis/canalis.sock` (`$XDG_RUNTIME_DIR/app/<id>/` in a Flatpak). It works the same way the chain protocol already does (serde types, one object per line). Up go `hello`, `graph`, `meters` and `bye`; down go `command` and `shutdown`. We chose this over D-Bus: a snapshot is a large tree sent on every change and meters arrive at 30 Hz, which suit a stream better than method calls and signals. zbus would add an async runtime for one peer type that we write ourselves. And a socket in the runtime dir is private to the user with no bus policy to ship. D-Bus stays the right choice for what other programs must reach: the tray (StatusNotifierItem, #45) and the Background portal.
- **Versions:** `hello` carries the protocol number and the build. A client only talks to a service of its own build. Otherwise it sends `shutdown` and starts the service again, once. `hello`, `shutdown` and `bye` never change shape.
- **Starting:** first through systemd, with a user unit that the window rewrites whenever its `ExecStart` names a binary other than the running one. In a Flatpak the `ExecStart` is `flatpak run --command=canalis <id>`, and `systemctl` goes through `flatpak-spawn --host`. Without a systemd user instance the service runs as a `setsid` child. "Run at login" is `systemctl --user enable`.
- **One writer:** only the service writes `config.toml`. `CANALIS_CONFIG` keeps a private in-process control plane for screenshot runs and tests.

## Consequences
Chains keep running across windows. After an update, the first window restarts the service once, so the chains restart once. A crashed service is restarted by systemd (`Restart=on-failure`) or by the next window. Flathub may refuse `--talk-name=org.freedesktop.Flatpak` with host `systemctl`; the fallback is the Background portal's autostart entry, which needs a manifest change and has not been tried.
