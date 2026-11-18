<!-- Part of the Canalis manual: one file per page, the same pages as https://claritas.meltingbots.com/canalis/docs. See docs/manual/MAINTAINING.md. -->

# Plugins

The plugins that come with Canalis, how to add more, and what happens when one misbehaves.

## Adding a plugin

**+ Add slot** offers:

- **Claritas EQ** and **Claritas Comp**, the plugins that come with Canalis. Each has its
  own window; Comp passes audio through until Claritas finishes it.
- **Plugin…**, which opens the catalogue.

If you build Claritas yourself, your copy in `~/.clap` is used instead of the one that came
with Canalis.

## The catalogue

The catalogue lists:

- every CLAP plugin found in `$CLAP_PATH`, `~/.clap`, `/usr/lib/clap`, `/usr/lib64/clap`
  and Canalis's own folder;
- every LV2 plugin lilv finds.

You can search it by name, vendor or id. **Rescan** picks up new installs.

The last eight plugins you picked sit at the top under **Recent**, newest first. The group
is hidden while you search, since the matches are listed anyway. One no longer installed
simply drops out of the group.

## Getting more plugins

Canalis needs no plugins to run: Claritas EQ and Comp come with it. To get more, open
**⚙ › Plugins** (or **Get more plugins** in the catalogue).

It lists the plugin families your distribution carries (LSP, Calf, ZAM, x42, Dragonfly
Reverb), each with a button:

- **Install**: the package installs through PackageKit and Canalis rescans by itself.
  PackageKit is the service behind GNOME Software and Discover, so dnf, apt, zypper and
  pacman all work.
- **Remove**: on an installed family, uninstalls its packages after asking you to confirm.

Your desktop asks for your password unless your distribution lets administrators install
without one (Fedora does, for users in `wheel`). Without PackageKit the row shows the
command to run instead.

## Dropping files on the window

You can also drop a `.clap` file or an LV2 bundle folder (`something.lv2`) anywhere on the
window. Canalis copies it into `~/.clap` or `~/.lv2` and rescans.

## LV2 support

LV2 plugins also need the lilv library (and suil for their windows). Without it they are
not listed and the **LV2 is off** row offers **Install**; everything else keeps working.

## Noise suppression

**Noise suppression** is not packaged by distributions, so its row has **Download**
instead. Canalis then:

1. fetches Alt Denoiser 1.0.3 (DeepFilterNet3, GPLv3) from GitHub, about 130 MB;
2. checks it against the hash it ships with;
3. puts its LV2 bundle in `~/.lv2` and rescans.

No password is needed; it needs LV2 on.

> [!TIP]
> Put it first on a mic chain, before the gate. It adds 40 ms of latency, which you
> notice only when listening to yourself.

## Laptops with efficiency cores

On laptops with performance and efficiency cores (recent Intel), power savers such as
`intel_lpmd` keep every app on the efficiency cores in the balanced and power-saver
profiles. A heavy plugin such as the denoiser can then miss the audio deadline.

When a chain overruns its cycle three times in a row, it bypasses its slowest slot on its
own, before the system would stop the whole chain. The slot then reads **bypassed, too slow
for this CPU** with its time and the cycle's. Turn the bypass off once the CPU can keep up.

**⚙ › Plugins** shows **Efficiency cores only** while that is the case. To fix it, either:

- use the performance profile while you need the denoiser, or
- turn the daemon off with `sudo systemctl disable --now intel_lpmd`.

## Installed plugins

**⚙ › Plugins › Installed** lists every plugin by where it came from, searchable:

- the Claritas pair that comes with Canalis;
- files in your own folders (**Remove** deletes them);
- system packages (**Uninstall** removes the package, after asking you to confirm).

Chains that used a removed plugin show its slot as missing.

> [!NOTE]
> Inside the Flatpak, system packages do not reach Canalis, so there is no
> Install. Dropping files on the window still works.

## LV2 presets

LV2 plugins get the same window button when they ship a UI (LSP does). They also get a
**P** button listing the factory presets they ship; pick one and the slot starts over from
it. What you change afterwards is kept with the chain, presets or not.

## When a plugin crashes

Plugins run in the chain's own process. If one crashes, the desktop keeps playing through
the restarted chain:

- the slot it crashed in is not loaded at all (its card says "off after a crash");
- the others are bypassed;
- the header says which slot it was.

Un-bypass what you trust.

If the chain still crashes three times in a row within seconds of starting (a plugin broken
by an update, say), Canalis stops restarting it and says so. **Start normally** in the
header starts it again with every slot loaded.

## Headphone correction

**+ Add slot → Headphone correction…** lists the headphones AutoEQ has measured (several
thousand models, from oratory1990, crinacle and others).

1. Type part of yours and pick it. A Claritas EQ slot named after it is added with AutoEQ's
   correction curve and preamp, so the headphones sound neutral.
2. Put it on the chain your headphones play from (an output chain such as Desktop EQ).
3. Open the slot's window to see or adjust the curve.

The list is fetched from GitHub once a month and kept for when you are offline.

<!-- nav: generated by scripts/manual.py -->

---

← [Chains](chains.md) · [Contents](README.md) · [Recipes](recipes.md) →
