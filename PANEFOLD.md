# What this fork changes, and why it exists

This is [gpui](https://crates.io/crates/gpui) 0.2.2 — the last published
release — with four X11 window bugs fixed, a lost Vulkan surface (section 5) made
again instead of taking the application down, and a lost GPU device (section
7) handed to the application instead of a panic, and a dead X server (section
8) ending the application instead of spinning it, test windows that report
only what their last frame drew (section 9), and a tooltip too wide for either
side of the pointer kept against the window's edge instead of thrown to its
far side (section 10). The base commit is the crates.io
tarball, unmodified, so `git diff <base>..HEAD` is the whole divergence: four
files, about 200 lines, most of them comment.

It exists because 0.2.2 was published on 2025-10-22 and nothing has been
published since, while all four bugs are still present in `zed-industries/zed`
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

## 4. A minimized window stopped calling itself maximized

`is_maximized` read `!hidden && maximized_vertical && maximized_horizontal`,
with a comment one line above saying the opposite of what the code did: "a
maximized window that gets minimized will still retain its maximized state".
`hidden` is `_NET_WM_STATE_HIDDEN`, which is what being minimized looks like on
X11, and EWMH keeps it *next to* `_MAXIMIZED_VERT` and `_MAXIMIZED_HORZ`, not
instead of them. Measured on the panefold window: maximized it carries

    _NET_WM_STATE_MAXIMIZED_HORZ, _NET_WM_STATE_MAXIMIZED_VERT

and minimized it carries

    _NET_WM_STATE_HIDDEN, _NET_WM_STATE_MAXIMIZED_HORZ, _NET_WM_STATE_MAXIMIZED_VERT

with the geometry unchanged at `+66+69 2494x1371` through both. So minimizing
turned `window_bounds()` from `Maximized` into `Windowed` around a rectangle
the size of the screen, and an application that saves its window would reopen
maximized windows as plain ones.

Fixed by dropping `hidden` from the condition. It is the only thing that ever
read the field — in 0.2.2 and on zed `main` alike — so the field is kept,
documented and `#[allow(dead_code)]`: the one job it is genuinely right for is
telling `visibility()` that an iconified window is not visible, which is a
different bug, filed as zed#64388.

A real unmaximize is still told apart, because it hands back the smaller
rectangle: measured, `+300+650 1050x720` with an empty `_NET_WM_STATE`.

## 5. A lost Vulkan surface killed the application

blade-graphics 0.7.1, the version gpui 0.2.2 renders with on Linux, answered
`VK_ERROR_SURFACE_LOST_KHR` with a panic in three places: `acquire_frame`
(`panic!("Aquire image error ...")`), a present (`check_gpu_crash`:
`panic!("GPU error ...")`) and `reconfigure_surface` (`unwrap` on every
surface query and on `create_swapchain`). A surface is lost when the window
system takes it away - a driver reset, a display going away, and on lavapipe
under Xvfb now and then on a fresh server: panefold's headless stand died with
`Aquire image error ERROR_SURFACE_LOST_KHR` on 2026-09-22 and 2026-09-26, and
twelve stop/start/run cycles on 2026-09-27 did not bring it back.

blade `main` (checked 2026-09-27) logs the acquire error instead of
panicking, but still panics on the present, and a logged error alone would
leave the window frozen: a lost surface stays lost, every later acquire fails
the same way, and gpui never makes a new one.

So `vendor/blade-graphics` is 0.7.1 (the commit before this one is the
crates.io tarball, unmodified) with:

- `Surface::is_lost()`; `acquire_frame`, a present and `reconfigure_surface`
  mark the surface lost and carry on instead of panicking. Other acquire
  errors are logged, as on blade `main`.
- `Context::recreate_surface(&mut surface, &window, config)`: a new
  `VkSurfaceKHR` for the same window, the old swapchain destroyed first (a
  window has one swapchain at a time), then configured. On failure the old
  surface stays, lost, and the call can be repeated.
- `create_surface` no longer leaks the `VkSurfaceKHR` it rejects - once per
  retry of a lost surface otherwise.
- `BLADE_FAKE_SURFACE_LOST=N` makes every Nth acquire report a lost surface
  and says so on stderr, which is how the recovery below was measured.

gpui's `BladeRenderer` keeps the window's raw handles and, in `draw`, acquires
the frame *before* the command encoder starts: on a lost surface it makes the
surface again and acquires once more; if that fails too the frame is skipped
without a half-recorded command buffer, and the next draw tries again.
Pipelines are rebuilt when the new surface's format or alpha differ.

Measured on 2026-09-27, panefold on lavapipe under Xvfb with
`BLADE_FAKE_SURFACE_LOST=5`: 1823 lost surfaces in about two minutes of
pointer motion, each one made again, the window still redrawing (its CPU
temperature footer changing between shots), resident memory flat at
153-156 MB and 43 open files throughout.

Cargo ignores `[patch]` in a dependency, so a crate using this fork patches
crates.io's `blade-graphics` to this repository itself, at the same rev:

    gpui = { git = "https://github.com/Yahook/gpui.git", rev = "..." }
    blade-graphics = { git = "https://github.com/Yahook/gpui.git", rev = "..." }

Built on its own, the fork's `Cargo.toml` does the same with a path.

## 6. A stale swapchain froze the window until it was resized

`VK_ERROR_OUT_OF_DATE_KHR` means the swapchain no longer matches the surface,
and the specification wants a new one: asking the old one again fails again.
blade-graphics 0.7.1 answered it with a frame that has no image, gpui drew
into `frames[0]` without having acquired it, and the present silently did
nothing. gpui only reconfigures the swapchain from `update_drawable_size*` (a
resize) and `update_transparency`, so a swapchain that went stale without a
change of size - a suspend and resume, a monitor change, a restore from
minimized, which on Linux never calls `update_drawable_size_even_if_unchanged`
- left the window standing still until it was dragged by an edge.

`vendor/blade-graphics` now keeps the fact:

- `Surface::is_out_of_date()`, set by an out-of-date acquire and cleared only
  by `reconfigure_surface`; until then `acquire_frame` hands out frames without
  an image without asking the swapchain again.
- `Frame::is_presentable()`: whether the frame holds an acquired image.
- `BLADE_FAKE_OUT_OF_DATE=N` makes every Nth acquire find the swapchain out of
  date, which then stays so - as a real one does - and says so on stderr.

`BladeRenderer::acquire_frame` reconfigures an out-of-date surface at its
current size and acquires once more, and skips a draw that still has no image
instead of drawing into one it does not own.

Measured on 2026-09-28, panefold on lavapipe under Xvfb. Without the renderer
half, `BLADE_FAKE_OUT_OF_DATE=40` froze the window at the first stale
swapchain: seven shots over 40 seconds with the pointer on different rows,
all byte for byte the same, no hover, the temperature footer standing still.
With it, `BLADE_FAKE_OUT_OF_DATE=5`: 1105 stale swapchains in about two
minutes of pointer motion, each one reconfigured, hover and footer drawn,
resident memory 157-168 MB without a trend and 53-55 open files; together
with `BLADE_FAKE_SURFACE_LOST=7`, 443 stale and 253 lost surfaces, the window
still drawing.

## 7. A lost GPU device panicked, or hung the renderer

`VK_ERROR_DEVICE_LOST` - a driver reset, a driver updated underneath, a resume
the driver did not survive - leaves nothing made from the device usable: the
context gpui shares between windows, the atlas, the pipelines. blade-graphics
0.7.1 met it in three places:

- a submit or a present (`check_gpu_crash`): `panic!("GPU has crashed ...")`;
- an acquire, after section 5: logged as any other error, a frame without an
  image, and the same failure on every frame after it - a window frozen for
  good;
- `wait_for`: `false`, as for "not done yet", and gpui's `wait_for_gpu` waits
  for a lost device in `while !wait_for(..) {}` - a hang at 100% CPU.

Recovering in place would mean rebuilding everything above; an application
can start itself again instead. `vendor/blade-graphics` now sends all three to
one place:

- `blade_graphics::set_device_lost_hook(fn(&str))`: what runs on a lost device,
  told where it was seen. A hook that returns is followed by the panic 0.7.1
  had, so an application that sets none behaves as before, less the freeze and
  the hang.
- `BLADE_FAKE_DEVICE_LOST=N` makes every Nth submit report a lost device, and
  says so on stderr.

gpui itself is unchanged; panefold sets the hook and `exec`s itself, as its
Restart does (`src/gpu_lost.rs`).

Measured on 2026-09-28, panefold on lavapipe under Xvfb with
`BLADE_FAKE_DEVICE_LOST=40`: three lost devices, three restarts under the same
pid, each window drawing again 73-89 ms after its image started (plus a one
second pause panefold leaves the driver); the fourth within five minutes made
panefold give up with exit code 75 and a line saying why. No panic in the log.

## 8. A dead X server left the application spinning at full CPU

`x11/client.rs`, `process_x11_events`: an error from `poll_for_event` was
logged and the loop went on. When the X server dies or restarts, the
connection's fd stays readable at EOF, so the level-triggered calloop source
woke the loop again at once, forever: the process never quit and burnt a core
until killed, logging `X11 connection: IOError` with a dedup counter in the
millions.

Fixed as `zed-industries/zed` `main` already has it
(`crates/gpui_linux/src/linux/x11/client.rs`, checked 2026-09-30): an
`IoError` returns `Err` from the event source, which ends `run`, and the
platform's quit callback shuts the app down as `quit()` would. Other
connection errors are logged as before.

Measured on 2026-09-30, panefold on lavapipe under Xvfb, killing only Xvfb:
the process gone 0.1 s later. The same panefold on the previous rev was still
alive 5 s later at 100% of a core.

## 9. A test window reported elements no frame drew any more

`window.rs`, `Frame::clear` cleared hitboxes, the scene and the rest, but not
`debug_bounds`, the map `VisualTestContext::debug_bounds` reads. The window
swaps two frames, so each buffer kept every `debug_selector` ever painted into
it: `debug_bounds(sel).is_none()` failed for anything drawn once, and
`is_some()` passed for an element long gone. Test support only; nothing a user
sees.

Fixed as `zed-industries/zed` `main` has it (`crates/gpui/src/window.rs`,
checked 2026-10-09 at `f0b38ac`): `clear` empties the map, and since a cached
view is not painted again, each frame also keeps its selectors in paint order
(`debug_bounds_records`) and `reuse_paint` carries a reused view's over by the
`PaintIndex` range, as it does its cursor styles. A bare `clear` would have
lost the selectors of every cached view (panefold's rail is one).

Measured on 2026-10-09 against panefold: four tests asserting that an element
went away (a menu's mark, a row's pencil, Codex's `fast`, a search bar's note)
fail on the previous rev and pass on this one; all 1510 pass.

## 10. A tooltip with no room beside the pointer jumped to the window's far edge

`window.rs`, `prepaint_tooltip`: a tooltip that does not fit right of the
pointer goes left of it, and when that has no room either it was put at
`max(0, origin.x - right() - window_right)`. `origin.x - right()` is minus the
width, so the whole came to 0 every time, and the tooltip stood at the
window's left edge, far from the pointer, over whatever was there. The same
formula, with the same result, for the bottom edge.

Measured on panefold on 2026-09-28: a History window 1180 px wide, a tooltip
640 px wide (its `40rem` cap), the pointer at x≈540 - the tooltip landed at
x = 0, over the rail. A window wider than twice the tooltip never meets it,
which is why it hid.

Fixed by putting the tooltip against the edge it overflowed
(`window_right - width`, never below 0), in a function of its own
(`tooltip_bounds_in`) with unit tests: an 800 px window, a 600 px tooltip, the
pointer at 400 - x = 200, not 0. `zed-industries/zed` `main` still has the
formula (`crates/gpui/src/window.rs`, checked 2026-10-11 at `93d7456688`).

## Rebasing onto a newer gpui

`git diff 0aaf41f..HEAD -- src/` is the whole patch to gpui. Nothing touches a
public API except the added `PlatformWindow::set_maximized`, which has a
default body, so a new base only needs those files re-patched.
`git diff 3e65e9a..HEAD -- vendor/` is the patch to blade-graphics; a gpui
that has moved to another blade needs it carried there, or dropped once blade
recovers a lost surface and reconfigures an out-of-date one on its own, and
lets an application answer a lost device.

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
- Minimizing a maximized window does not unmaximize it. Iconify it without a
  mouse by sending the root window a `WM_CHANGE_STATE` client message with
  `IconicState` (3); `_NET_ACTIVE_WINDOW` brings it back. mutter does not
  unmap an iconified window, so map state is no help - read `_NET_WM_STATE`.
- Nothing jumps at startup. Sample the position every few milliseconds for the
  first seconds of a run: there should be exactly one placement, the right
  one. Before this fork there were two - mutter's own spot first, panefold's
  move about 230 ms later.

Measured this way on 2026-09-18, against panefold with its workarounds
deleted: one placement at +129 ms, a drag reaching `state.toml` unchanged, and
a window saved `900x600 maximized` coming back maximized and unmaximizing to
`900x600` where it was left. Bug 4 the same day: maximize, minimize, quit, and
`state.toml` still said `2494x1371 maximized = true`; reopened, the window came
back maximized.
