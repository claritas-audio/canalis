<!-- Part of the Canalis manual: one file per page, the same pages as https://claritas.meltingbots.com/canalis/docs. See docs/manual/MAINTAINING.md. -->

# Scenes, tray and control

Ways to drive Canalis without opening its window: scenes, the tray, MIDI controllers,
notifications, the command line and your compositor's keys.

## Scenes

**Scene ▾** at the top right keeps whole routing states: the defaults, where each app is
sent, every chain's ends, mutes and bypasses. A typical set: "Music", "Calls", "Streaming".

### Saving and recalling

- **Save current as…** names the state you have now.
- Clicking a scene recalls it, changing only what differs, so links that stay the same keep
  playing.
- Ctrl+Z undoes a recall.

### Recalling by itself

A scene can also recall itself. **when…** next to it offers:

- **When a device appears**: your headphones, a USB mic.
- **When an app starts**: Discord, OBS.
- **Only by hand**: turns the trigger off.

It fires each time that device is plugged in or that app starts playing or recording, not
for what was already there when Canalis started. Saving the scene again keeps its trigger.

## Tray

While the service runs, the canal mark sits in your desktop's tray. Click it to open the
window and again to close it (the chains keep running).

### The tray menu

- **Open Canalis**
- **Output** and **Input**: the default devices.
- **Scenes**
- **Mute microphone**: your default input's own mute, as the desktop's volume control sets
  it; the tooltip says so while it is on.
- **Mute all chains**
- **Quit Canalis**: stops the service and every chain with it, and closes open windows.

Canalis has one window: opening it again, from the tray or the launcher, brings the open
one forward.

### Tray hosts

The tray needs a StatusNotifierItem host:

- KDE Plasma has one.
- On GNOME, install the "AppIndicator and KStatusNotifierItem Support" extension.
- With niri or Sway, use a bar with a tray (Noctalia, Waybar's `tray` module).

> [!NOTE]
> With no host the service runs the same way, just without the icon, and the icon
> appears if a host starts later.

## MIDI control

### Picking a controller

Pick your controller under **MIDI control** in the **⚙** settings (anything with a MIDI
output port; the ALSA bridge lists USB controllers).

### Learning a control

1. Right-click what you want to control, or press **learn** next to a scene. You can
   right-click:
   - a chain's **M**
   - a slot's **B**
   - a send level
   - any plugin parameter under a slot's **edit**
   - a soundboard pad (or a whole bank, from the Pads tab)
2. Move a knob or hit a pad.

The message is mapped and shown in the list, with **×** to forget it. Mappings learned on a
controller that is not plugged in stay listed, dimmed.

### How messages act

- A note toggles.
- A CC above 63 switches on and below switches off.
- A CC spans a send level from off to +12 dB, and a plugin parameter over its whole range
  (a note flips it between its ends).

## Notifications

The service tells your desktop when something needs you, with the window closed too.
**Settings → General → Notifications** has a switch for each:

- **Crashes** (on): a chain crashed and comes back with its slots bypassed, or Canalis
  stopped restarting it after it crashed again and again. **Start normally** on the
  notification starts it again with every slot loaded, as the button in its strip does.
- **Clipping** (off): a chain's output keeps hitting full scale for a while. It is told
  once, and again only after ten seconds without a clip.
- **Lost devices** (off): a device a running chain plays into or listens to went away
  (headphones unplugged, a USB interface switched off).

Each subject is told at most once a minute, so a device that drops in and out does not
fill the screen. A private window (`CANALIS_CONFIG`) sends none.

## Command line

| | |
|---|---|
| `canalis` | the window |
| `canalis --daemon` | the service itself, until SIGTERM or `canalis quit` |
| `canalis scene` | list the saved scenes |
| `canalis scene <name>` | recall a scene |
| `canalis mute mic [on\|off\|toggle]` | the default input's own mute, as the tray's **Mute microphone** |
| `canalis mute chains [on\|off\|toggle]` | every chain's M, as the tray's **Mute all chains** |
| `canalis chain <name> mute\|bypass [on\|off\|toggle]` | one chain's M, or its B (every slot bypassed) |
| `canalis ptt mic\|<chain> press\|release\|hold` | push to talk: press opens, release mutes; `hold` for keys that only repeat |
| `canalis pad` | list the soundboard's pads |
| `canalis pad <name> [press\|release\|stop]` | press a pad (bind press and release for a Hold pad) |
| `canalis pad --stop` | stop every pad |
| `canalis quit` | stop the service and its chains |
| `canalis --install` / `--uninstall` | the user unit, desktop entry and icon |
| `CANALIS_VIEW=chains canalis` | open on a view: devices, apps, chains, graph |
| `CANALIS_CONFIG=file.toml canalis` | a private window with its own control plane and config file, no service |

### Arguments

`toggle` is the default. `<name>` is a chain's name as the strip shows it.

### Exit status

The scene, mute, chain and ptt commands talk to the running service and never start one.

- When it is not running, or a name is unknown, they print one line on stderr and exit
  with 1 (2 for a wrong command line).
- On success they print the new state.

## Keybinds

Your compositor runs these commands from a key, which also works where there is no global
shortcut portal (niri). `mic` is the default input; put a chain's name there instead to talk
through that chain (its M).

### Sway and Hyprland

Push to talk wants a bind on press and another on release:

```
# Sway
bindsym F13 exec canalis ptt mic press
bindsym --release F13 exec canalis ptt mic release
# Hyprland
bind = , F13, exec, canalis ptt mic press
bindr = , F13, exec, canalis ptt mic release
```

### niri

niri has no release binds yet, but a held key repeats, so bind `hold`. Every repeat keeps
the mic open, and it mutes again 0.7 s after you let go (just longer than niri's 600 ms
repeat delay; raise `repeat-delay` above that and the mic closes before the repeats start).
In `~/.config/niri/config.kdl`:

```kdl
binds {
    F13 { spawn "canalis" "ptt" "mic" "hold"; }
    Mod+F9 repeat=false { spawn "canalis" "mute" "mic"; }
    Mod+F10 repeat=false { spawn "canalis" "scene" "Calls"; }
}
```

### GNOME

Add the same commands as custom shortcuts under Settings › Keyboard. They fire on press
only, so use `mute mic` as a toggle.

<!-- nav: generated by scripts/manual.py -->

---

← [Soundboard](soundboard.md) · [Contents](README.md) · [Where things live](files.md) →
