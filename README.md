# MinkaShot

MinkaDE's screenshot tool, on AntiWebUI. The crate is a library: MinkaDE's one
binary, `minka`, runs it as `minkashot` (`/usr/bin/minkashot` is a link to
`/usr/bin/minka`, and `minka minkashot ...` does the same). Nothing is built
from here alone. `cargo build --profile release-fast -p minka` at the top of
MinkaDE builds it into `target/release-fast/minka`.

## Commands

```
minkashot full [<output>] [<path>]
minkashot region <output> <x,y,w,h> [<path>]
minkashot win [<window>] [<path>]
minkashot list
minkashot interactive | cancel | status
minkashot daemon
```

`full`, `region` and `win` take a screenshot of one output, of a region of it,
or of one window, save it as PNG and print the path written. They show nothing
and take no input, and give up on a compositor that does not answer. A window
is pictured as it is, even under others, and is not raised or focused. `list`
prints each window's app id and title, a tab between. `help`, `--help` and `-h`
print the usage. With no command it prints the usage as an error (status 2), so
nobody starts a daemon by accident.

- `<output>`: a name like `eDP-1` or `DP-1`. Left out or `""`, the focused
  output.
- `<x,y,w,h>`: the region, in the output's logical pixels from its top left.
- `<window>`: a role (`minkamon.disk`), a whole title, an app id, or a piece of
  a title or app id, tried in that order. Left out or `""`, the focused window.
  Of several that match, minimised ones count only if all are minimised; if
  more than one is left, the focused one, else none.
- `<path>`: where to save. Left out or `""`,
  `~/Pictures/Screenshots/minkashot-yyyyMMdd-hhmmss.png`, where a shot never
  replaces an earlier one. `-` writes the PNG to standard output instead and
  prints no path (`minkashot full "" - | wl-copy -t image/png`), and `./-` is a
  file.

## Exit statuses

| Status | Meaning |
|--------|---------|
| 0 | saved, listed or answered |
| 1 | the capture failed or timed out, or the daemon could not run |
| 2 | usage |
| 3 | no output or window of that name, or several windows and none focused |
| 4 | the file could not be written (a full disk, or a path that is a device, a pipe or a directory) |
| 5 | a command for the daemon, and no daemon is running |
| 6 | the daemon did not answer, or its lock could not be read |

A command that fails says why on standard error.

## The daemon

`minkashot daemon` is the interactive screenshot. One runs per session: it holds
`$XDG_RUNTIME_DIR/minkashot.lock`, and a second one exits at once with status 0.
Print arms it, through the compositor's `ui.minkashot` broadcast, and so does
`minkashot interactive`.

An arm freezes every output under an overlay showing its frame, with a
crosshair, a loupe and a toolbar. A drag makes a region, and so does a window
picked with the toolbar's Window. Enter saves the region of the output the
pointer is on, or all of it, under the default name. The toolbar's copy puts it
on the clipboard as `image/png` instead. Esc lets go. The toolbar's retry
freezes again after the delay it sets (0 to 10 s), counted down on screen, and
Print during the countdown freezes at once. A save or a copy that fails says why
on the overlay, and the frames stay up.

`minkashot cancel` puts the daemon back to idle, stopping a countdown too, and
`minkashot status` says what it is doing. Each prints the daemon's one-line
answer.

## Log

The session starts the daemon as `/usr/lib/minka/minka-keep minkashot daemon`.
minka-keep appends what it prints to `~/.local/state/minka/minkashot.log` and
starts it again if it dies. The daemon logs each Print, `interactive` and
`cancel`, each frame captured and each save or copy there, with the time.
