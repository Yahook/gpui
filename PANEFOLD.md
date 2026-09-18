# What this fork changes, and why it exists

This is [gpui](https://crates.io/crates/gpui) 0.2.2 — the last published
release — with three X11 window bugs fixed. The base commit is the crates.io
tarball, unmodified, so `git diff <base>..HEAD` is the whole divergence: four
files, about 190 lines, most of them comment.

It exists because 0.2.2 was published on 2025-10-22 and nothing has been
published since, while all three bugs are still present in `zed-industries/zed`
on `main` (checked 2026-09-18, where the Linux backend now lives in
`crates/gpui_linux/`). Waiting for a release was not a plan.

Everything below was measured on mutter / GNOME, X11, one 2560x1440 screen.

## 1. The window's origin was its inset into the frame, not its place on screen

`x11/client.rs`, `Event::ConfigureNotify` took `event.x` / `event.y` as the
window's position. A `ConfigureNotify` the server generates carries the
position inside the window's **parent**, and under a reparenting window
manager that parent is the frame. Measured: window at `+66+69`, frame at
`+52+20`, gpui reporting `14, 49`.

Two consequences. Dragging a window never changed the number, because the
window does not move inside its frame. And the number was not even stable: the
window manager also sends a *synthetic* `ConfigureNotify` in root coordinates,
gpui took both kinds, and whichever landed last won.

Fixed by translating to root coordinates for the server-generated event and
trusting the synthetic one (ICCCM 4.2.3). Two workarounds that existed only
because of this bug went with it:

- `set_bounds` threw the origin away on a resize "because it contains wrong
  values" — so a window dragged and resized in one gesture reported the
  position it had before.
- `X11Window::new` added two pixels to the requested x and nudged the window by
  two more when it had been placed at the origin, "to work around a bug where
  our rendered content appears outside the window bounds when opened at the
  default position (14px, 49px on X + Gnome + Ubuntu 22)". That default
  position *was* this bug. Left in, the two pixels walk a window right across
  the screen, two at a time, once a saved position is restored.

## 2. The position in `WindowOptions` was ignored

`WM_NORMAL_HINTS` was written only when `window_min_size` was set, and never
carried a position flag. ICCCM 4.1.2.3 leaves a window manager free to place a
window that does not claim its position is meant, and mutter does: measured,
the window landed at mutter's own `116, 119` whatever it was asked for.

Fixed by always writing the hints, with `UserSpecified` position and
`StaticGravity` — the gravity matters, or the position is the frame's and the
window opens a title bar lower than asked, every time.

## 3. `WindowBounds::Maximized` did not maximize

`Window::new` answers it with `zoom()`, about two hundred lines before it calls
`map_window()`. The X11 `zoom()` sends a `_NET_WM_STATE` message with action
`TOGGLE` — the only action in `WmHintPropertyState`, `Remove` and `Add` being
commented out — and a window manager does not read state messages about a
window it has not been given yet. A window asked to open maximized opened the
right size and plain.

`Toggle` is also the wrong verb here: mutter maximizes a window of its own
accord when the size asked for matches the work area, which the saved size of a
maximized window always does — so a toggle would undo it. (This is also how the
bug hid: the window looked maximized, because mutter had done it.)

Fixed with a `set_maximized(bool)` on `PlatformWindow`, defaulting to the old
`zoom()` so no other backend changes. X11 remembers a maximize asked for before
the map and sends it, as an **add**, from the `MapNotify` handler.

Measured, in order, so the next person does not repeat it: the property EWMH
prescribes for an unmapped window is written and ignored; a message sent
immediately after `MapWindow` is ignored too, because the window manager has
not taken the window yet; a message sent once `MapNotify` has arrived works.

## Rebasing onto a newer gpui

`git diff 0aaf41f..HEAD -- src/` is the whole patch. Nothing touches a public
API except the added `PlatformWindow::set_maximized`, which has a default body,
so a new base only needs the four files re-patched.

## Testing a change

panefold points at this fork through `[patch.crates-io]` and carries no window
placement workarounds of its own any more - what the window does is this
fork's doing and nothing else. Four things to check, measured on mutter /
GNOME, X11:

- A saved position opens where it was saved, and survives a restart. Read the
  CLIENT window, not the frame: `xwininfo -name 'panefold (dev)'` answers with
  mutter's frame, which sits 14 px left and 49 px above the window and is
  wider and taller by the decorations. `xwininfo -root -tree | grep
  panefold-dev` is the line that means the window.
- Dragging the window updates the saved position. panefold records it from
  gpui's `moved` callback while rendering, so a move that is not also a resize
  has to arrive.
- A window saved maximized comes back with `_NET_WM_STATE_MAXIMIZED_HORZ` and
  `_VERT` in `xprop`. Save it at a size **unlike** the work area, or the test
  proves nothing: mutter maximizes a window of its own accord when the size
  asked for matches, and that is how bug 3 hid for a while.
- Nothing jumps at startup. Sample the position every few milliseconds for the
  first seconds of a run: there should be exactly one placement, the right
  one. Before this fork there were two - mutter's own spot first, panefold's
  move about 230 ms later.

Measured this way on 2026-09-18, against panefold with its workarounds
deleted: one placement at +129 ms, a drag reaching `state.toml` unchanged, and
a window saved `900x600 maximized` coming back maximized and unmaximizing to
`900x600` where it was left.
