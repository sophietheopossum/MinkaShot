//! One output's overlay while the frames are up: its frozen frame filling
//! the layer, pixel for pixel, and what the QML CaptureOverlay drew over it
//! to choose with. A drag makes a region, with the scrim round it, a red
//! outline and its size; a crosshair runs through the pointer, which is
//! hidden; the loupe shows the pixels round the pointer; and while picking a
//! window, the one a press would take is lit. The toolbar (`toolbar.rs`) is
//! at the top, and a save or a copy that failed is said at the bottom.
//! Presses on either are theirs, not the frame's.
//!
//! The keyboard's focus stays on the overlay itself, whatever is clicked:
//! Esc and Enter must always reach it, or the frozen screen would stick.
//!
//! Pointer positions come in the surface's own pixels. Regions, readouts
//! and the compositor's window rects are in the output's logical pixels, a
//! region from the output's top left. [`Spot`] is where the two meet.
//!
//! What it hears goes to the daemon as [`Told`], through the daemon's inbox;
//! it never steps the machine itself (see `Daemon::carry_out`). The region
//! is the overlay's own, as each QML overlay's was, and it puts it where the
//! daemon reads it for Enter.

use anti_webui::core::animation::request_frame;
use anti_webui::core::event::{Key, NamedKey, PointerButton, PointerEvent};
use anti_webui::core::images::Bitmap;
use anti_webui::core::prelude::{Div, Signal, bitmap_view, div, node_ref, signal, window_scale};
use anti_webui::core::reactive::{effect, untrack};
use anti_webui::core::style::{CursorIcon, ImageRendering, Length, Style, Styled};
use anti_webui::core::window::WindowId;
use anti_webui_md3e::components::{Snackbar, snackbars};
use std::cell::Cell;
use std::rc::Rc;
use std::sync::Arc;
use std::time::Instant;

use crate::geometry::{Rect, js_round};
use crate::state::Event;
use crate::theme::{self, FONT_SIZE, GROUND, RED, TEXT_MUTED, color, mono_text};
use crate::toolbar::{self, Controls};
use crate::window::{self, WindowRect};

/// A region smaller than this on either side, in logical pixels, is let go
/// of on release: a click is not a region.
const MIN_REGION: f64 = 4.0;
/// The loupe shows this many logical pixels a side round the pointer...
const LOUPE_CELLS: u32 = 21;
/// ...each this many logical pixels wide...
const LOUPE_CELL: f32 = 6.0;
/// ...this far from the pointer, with this much under the cells for its
/// readout.
const LOUPE_GAP: f64 = 24.0;
const LOUPE_FOOT: f32 = 18.0;
/// The loupe's cells, and the whole loupe with its 1 px border.
const LOUPE_BODY: f32 = LOUPE_CELLS as f32 * LOUPE_CELL;
const LOUPE_WIDTH: f32 = LOUPE_BODY + 2.0;
const LOUPE_HEIGHT: f32 = LOUPE_BODY + 2.0 + LOUPE_FOOT;
/// Between a region's or a window's edge and its label.
const LABEL_GAP: f32 = 4.0;
/// The outlines and the crosshair: a logical pixel, as the QML's.
const LINE: f32 = 1.0;

/// What an overlay tells the daemon.
#[derive(Debug, PartialEq)]
pub enum Told {
    /// Esc, Enter, the toolbar, a window picked, or the pointer coming onto
    /// it.
    Event(Event),
    /// Its first frame was about to be drawn at `drawn`, and the next one
    /// began at `next`: about when the first was on screen, if presenting
    /// it went through. A present that failed is retried 4 ms later instead.
    Drawn {
        output: String,
        drawn: Instant,
        next: Instant,
    },
    /// Its layer is gone: closed by the daemon, or it could not open or
    /// failed.
    Closed(WindowId),
}

/// What an overlay shows: an output's frozen frame, and the windows that
/// could be picked on it.
pub struct Frozen {
    pub output: String,
    /// Where the output is on the desktop, in logical pixels, where the
    /// window rects are.
    pub position: (f64, f64),
    pub frame: Arc<Bitmap>,
    /// The windows at the freeze, every output's, top first.
    pub windows: Vec<WindowRect>,
}

/// What every overlay shows the same, from the machine: signals the daemon
/// keeps, which outlive the layers.
#[derive(Clone, Copy)]
pub struct Shared {
    /// Whether a press picks a window.
    pub pick: Signal<bool>,
    /// The seconds before a retry.
    pub delay: Signal<u8>,
    /// The last save or copy that failed, for the overlay it names to say.
    pub failure: Signal<Option<Failure>>,
}

/// A save or a copy that failed: what to say, and on which output.
#[derive(Clone, Debug, PartialEq)]
pub struct Failure {
    pub output: String,
    pub text: String,
}

/// Where the pointer is on an overlay.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Spot {
    /// In the output's logical pixels, from its top left: off the output
    /// while a drag goes past its edge.
    pub x: f64,
    pub y: f64,
    /// The output's logical size.
    pub width: f64,
    pub height: f64,
    /// The same point in the surface's own pixels, and the surface's size
    /// in them.
    physical: (f64, f64),
    surface: (f64, f64),
}

impl Spot {
    /// From a position on the surface and its size, both in its own pixels,
    /// at `scale`.
    fn new(physical: (f64, f64), surface: (f64, f64), scale: f64) -> Spot {
        Spot {
            x: physical.0 / scale,
            y: physical.1 / scale,
            width: surface.0 / scale,
            height: surface.1 / scale,
            physical,
            surface,
        }
    }

    /// Where an event on the overlay's root, which fills the surface, is.
    fn of(ev: &PointerEvent) -> Spot {
        Spot::new(
            (ev.local.x as f64, ev.local.y as f64),
            (ev.rect.width as f64, ev.rect.height as f64),
            window_scale().as_f64(),
        )
    }

    fn at(&self) -> (f64, f64) {
        (self.x, self.y)
    }
}

/// A window a press would pick: its rect on this output, in the output's
/// logical pixels, and its title.
#[derive(Clone, Debug, PartialEq)]
struct Hover {
    rect: Rect,
    title: String,
}

/// The loupe's picture: the frame round the pointer, [`LOUPE_CELLS`]
/// logical pixels a side, a pixel of the frame for each, and the one under
/// the pointer in the middle. A cell is a logical pixel, as in the QML, so
/// at scale 2 it shows every other pixel of the frame. Cells off the frame
/// are clear, for the loupe's ground to show. `None` if the frame's pixels
/// are gone.
fn loupe_cells(frame: &Bitmap, spot: &Spot) -> Option<Bitmap> {
    let (fw, fh) = (frame.width() as usize, frame.height() as usize);
    if spot.surface.0 <= 0.0 || spot.surface.1 <= 0.0 {
        return None;
    }
    // The pointer in the frame's pixels, and how many of them there are to
    // a logical pixel. The frame fills the surface pixel for pixel, but the
    // sums do not lean on it.
    let at = (
        spot.physical.0 * fw as f64 / spot.surface.0,
        spot.physical.1 * fh as f64 / spot.surface.1,
    );
    let step = (fw as f64 / spot.width, fh as f64 / spot.height);
    let n = LOUPE_CELLS as usize;
    let middle = (n / 2) as f64;
    let mut cells = vec![0; n * n * 4];
    let read = frame.with_levels(|level, _, pixels| {
        if level != 0 || pixels.len() < fw * fh * 4 {
            return;
        }
        for row in 0..n {
            let y = (at.1 + (row as f64 - middle) * step.1).floor();
            if !(0.0..fh as f64).contains(&y) {
                continue;
            }
            for col in 0..n {
                let x = (at.0 + (col as f64 - middle) * step.0).floor();
                if !(0.0..fw as f64).contains(&x) {
                    continue;
                }
                let from = (y as usize * fw + x as usize) * 4;
                let to = (row * n + col) * 4;
                cells[to..to + 4].copy_from_slice(&pixels[from..from + 4]);
            }
        }
    });
    read.then(|| Bitmap::from_premultiplied(LOUPE_CELLS, LOUPE_CELLS, cells, false))
}

/// Where the loupe goes for a pointer at `spot`, as the QML put it: below
/// and right of it, and above or left of it where it would leave the output.
fn loupe_at(spot: &Spot) -> (f64, f64) {
    let side = |at: f64, size: f32, room: f64| {
        let size = size as f64;
        if at + LOUPE_GAP + size > room {
            at - LOUPE_GAP - size
        } else {
            at + LOUPE_GAP
        }
    };
    (
        side(spot.x, LOUPE_WIDTH, spot.width),
        side(spot.y, LOUPE_HEIGHT, spot.height),
    )
}

/// The loupe's readout: the logical pixel under the pointer, and the
/// region's size when there is one.
fn readout(spot: &Spot, region: Option<Rect>) -> String {
    let at = format!("{}, {}", spot.x.floor(), spot.y.floor());
    match region {
        Some(r) => format!("{at}  ·  {}", size(&r)),
        None => at,
    }
}

/// `W×H`, rounded as the QML's `Math.round`.
fn size(r: &Rect) -> String {
    format!("{}×{}", js_round(r.width), js_round(r.height))
}

/// A box of chrome over the frame, which the pointer goes through.
fn chrome() -> Div {
    div().absolute().pointer_events_none()
}

/// A label in mono at a point of the frame, the corner `place` says of the
/// text there: a box of no size there, holding the text.
fn label(
    text: impl Fn() -> String + 'static,
    size: f32,
    hex: u32,
    place: impl Fn(Style) -> Style + 'static,
    corner: impl FnOnce(Div) -> Div,
) -> Div {
    chrome()
        .w(0.0)
        .h(0.0)
        .dyn_style(place)
        .child(corner(chrome().whitespace_nowrap()).child(mono_text(text, size, hex)))
}

/// The overlay of `frozen.output`. It takes the keyboard's focus as it
/// opens, again whenever the pointer comes onto it, and back from whatever
/// takes it: with nothing focused, keys reach no handler of the program's.
/// `shared` is the machine's pick mode and delay, and the failures to say;
/// `region` gets the region, for Enter and copy.
pub fn view(
    frozen: Frozen,
    shared: Shared,
    region_out: Rc<Cell<Option<Rect>>>,
    tell: Rc<dyn Fn(Told)>,
) -> Div {
    view_with(frozen, shared, region_out, tell, Controls::new())
}

/// [`view`], with the toolbar's buttons where the tests find them.
fn view_with(
    frozen: Frozen,
    shared: Shared,
    region_out: Rc<Cell<Option<Rect>>>,
    tell: Rc<dyn Fn(Told)>,
    controls: Controls,
) -> Div {
    let Frozen {
        output,
        position,
        frame,
        windows,
    } = frozen;
    let pick = shared.pick;
    let root = node_ref();
    request_frame(move |_| root.focus());

    // A failure for this output, said at the bottom for 10 s; the overlay
    // stays as it was. The daemon clears it as the overlays close, so a new
    // one never starts with an old failure. No close button: clicked, it
    // would hold the focus and take it away as it went, Esc and Enter too.
    let bars = snackbars();
    {
        let output = output.clone();
        effect(move || {
            if let Some(f) = shared.failure.get().filter(|f| f.output == output) {
                untrack(|| bars.show(Snackbar::new(f.text).long()));
            }
        });
    }

    let pointer: Signal<Option<Spot>> = signal(None);
    let region: Signal<Option<Rect>> = signal(None);
    let hover: Signal<Option<Hover>> = signal(None);
    // Where the drag on now began.
    let origin: Rc<Cell<Option<(f64, f64)>>> = Rc::default();
    let set_region = move |r: Option<Rect>| {
        region.set_if_changed(r);
        region_out.set(r);
    };
    // The window a press at `spot` would pick, in this output's pixels.
    let window_at = {
        let output = output.clone();
        move |spot: &Spot| {
            let at = (position.0 + spot.x, position.1 + spot.y);
            window::window_at(&windows, &output, at).map(|w| Hover {
                rect: Rect {
                    x: w.rect.x - position.0,
                    y: w.rect.y - position.1,
                    ..w.rect
                },
                title: w.title.clone(),
            })
        }
    };
    let window_at = Rc::new(window_at);

    let (keys, picked, came, buttons) = (tell.clone(), tell.clone(), tell.clone(), tell);
    let (drag, pressed) = (origin.clone(), origin.clone());
    let (drag_to, press_to) = (set_region.clone(), set_region.clone());
    let pick_at = window_at.clone();
    let shown = frame.clone();
    let root_div = div()
        .node_ref(root)
        .focusable()
        .size_full()
        .relative()
        // The crosshair and the loupe stand in for the pointer.
        .cursor(CursorIcon::None)
        .select_none()
        // A click on a toolbar button focuses the button: the overlay takes
        // it straight back, so Esc and Enter stay its own and nothing
        // else's. A button turned off with the focus gives it back itself
        // (`toolbar.rs`).
        .on_focus_change(move |on| {
            if !on {
                root.focus();
            }
        })
        .on_key_down(move |ev| {
            // Keypad Enter comes as Enter (anti-webui-wayland's keyboard.rs).
            let event = match ev.key {
                Key::Named(NamedKey::Escape) => Event::Cancel,
                Key::Named(NamedKey::Enter) => Event::Save,
                _ => return,
            };
            // Enter would otherwise click whatever has the focus.
            ev.prevent_default();
            ev.stop_propagation();
            keys(Told::Event(event));
        })
        // Once each time the pointer comes onto it: the keyboard follows.
        .on_hover(move |on| {
            if on {
                root.focus();
                came(Told::Event(Event::Pointer(output.clone())));
            } else {
                pointer.set(None);
                hover.set(None);
            }
        })
        .on_pointer_move(move |ev| {
            let spot = Spot::of(ev);
            pointer.set(Some(spot));
            if let Some(from) = drag.get() {
                drag_to(Some(
                    Rect::between(from, spot.at()).clamped(spot.width, spot.height),
                ));
            }
            let picking = pick.get_untracked() && drag.get().is_none();
            hover.set_if_changed(if picking { window_at(&spot) } else { None });
        })
        // Only the left button, as in the QML.
        .on_pointer_down(move |ev| {
            if ev.button != Some(PointerButton::Primary) {
                return;
            }
            let spot = Spot::of(ev);
            pointer.set(Some(spot));
            if pick.get_untracked() {
                // A window is the region; empty desktop is nothing.
                if let Some(w) = pick_at(&spot) {
                    press_to(Some(w.rect.clamped(spot.width, spot.height)));
                    hover.set(None);
                    picked(Told::Event(Event::Picked));
                }
                return;
            }
            // A new press lets the old region go.
            pressed.set(Some(spot.at()));
            press_to(None);
        })
        .on_pointer_up(move |ev| {
            if ev.button != Some(PointerButton::Primary) {
                return;
            }
            let spot = Spot::of(ev);
            if let Some(from) = origin.take() {
                set_region(Some(
                    Rect::between(from, spot.at()).clamped(spot.width, spot.height),
                ));
            }
            let small = |r: Rect| r.width < MIN_REGION || r.height < MIN_REGION;
            if region.get_untracked().is_some_and(small) {
                set_region(None);
            }
        })
        .child(bitmap_view(move || Some(shown.clone())).size_full());

    // The scrim, in four pieces round the region.
    let scrim = |place: fn(Style, Rect) -> Style| {
        chrome()
            .bg(theme::scrim())
            .dyn_style(move |s| match region.get() {
                Some(r) => place(s, r),
                None => s.hidden(),
            })
    };
    let above = scrim(|s, r| s.left(0.0).right(0.0).top(0.0).h(r.y as f32));
    let below = scrim(|s, r| {
        s.left(0.0)
            .right(0.0)
            .top((r.y + r.height) as f32)
            .bottom(0.0)
    });
    let left = scrim(|s, r| s.left(0.0).top(r.y as f32).w(r.x as f32).h(r.height as f32));
    let right = scrim(|s, r| {
        s.left((r.x + r.width) as f32)
            .right(0.0)
            .top(r.y as f32)
            .h(r.height as f32)
    });

    // The region's outline, and its size under its right edge.
    let outline = chrome()
        .border(LINE)
        .border_color(color(RED))
        .dyn_style(move |s| match region.get() {
            Some(r) => place(s, &r),
            None => s.hidden(),
        });
    let region_size = label(
        move || region.get().map(|r| size(&r)).unwrap_or_default(),
        FONT_SIZE - 2.0,
        RED,
        move |s| match region.get() {
            Some(r) => s
                .left((r.x + r.width) as f32)
                .top((r.y + r.height) as f32 + LABEL_GAP),
            None => s.hidden(),
        },
        |d| d.right(0.0).top(0.0),
    );

    // The window a press would pick, lit, with its title above its left edge.
    let picking = move || if pick.get() { hover.get() } else { None };
    let lit = chrome()
        .bg(theme::pick_fill())
        .border(LINE)
        .border_color(color(RED))
        .dyn_style(move |s| match picking() {
            Some(w) => place(s, &w.rect),
            None => s.hidden(),
        });
    let title = label(
        move || picking().map(|w| w.title).unwrap_or_default(),
        FONT_SIZE - 2.0,
        RED,
        move |s| match picking() {
            Some(w) => s.left(w.rect.x as f32).top(w.rect.y as f32 - LABEL_GAP),
            None => s.hidden(),
        },
        |d| d.left(0.0).bottom(0.0),
    );

    // The crosshair, through the surface's pixel under the pointer.
    let across = chrome()
        .bg(theme::crosshair())
        .left(0.0)
        .right(0.0)
        .h(LINE)
        .dyn_style(move |s| match pointer.get() {
            Some(p) => s.top(Length::Device(p.physical.1.floor() as i32)),
            None => s.hidden(),
        });
    let down = chrome()
        .bg(theme::crosshair())
        .top(0.0)
        .bottom(0.0)
        .w(LINE)
        .dyn_style(move |s| match pointer.get() {
            Some(p) => s.left(Length::Device(p.physical.0.floor() as i32)),
            None => s.hidden(),
        });

    // The loupe: the cells, the one under the pointer outlined, and where
    // it is under them.
    let cells = bitmap_view(move || {
        let spot = pointer.get()?;
        loupe_cells(&frame, &spot).map(Arc::new)
    })
    .absolute()
    .left(0.0)
    .top(0.0)
    .size(LOUPE_BODY)
    .image_rendering(ImageRendering::Pixelated);
    let middle = (LOUPE_CELLS / 2) as f32 * LOUPE_CELL;
    let centre = chrome()
        .left(middle)
        .top(middle)
        .size(LOUPE_CELL)
        .border(LINE)
        .border_color(color(RED));
    let foot = chrome()
        .left(0.0)
        .right(0.0)
        .bottom(3.0)
        .flex()
        .justify_center()
        .whitespace_nowrap()
        .child(mono_text(
            move || {
                pointer
                    .get()
                    .map(|p| readout(&p, region.get()))
                    .unwrap_or_default()
            },
            FONT_SIZE - 4.0,
            TEXT_MUTED,
        ));
    let loupe = chrome()
        .w(LOUPE_WIDTH)
        .h(LOUPE_HEIGHT)
        .bg(color(GROUND))
        .border(LINE)
        .border_color(color(RED))
        .dyn_style(move |s| match pointer.get() {
            Some(p) => {
                let (x, y) = loupe_at(&p);
                s.left(x as f32).top(y as f32)
            }
            None => s.hidden(),
        })
        .child(cells)
        .child(centre)
        .child(foot);

    // The toolbar at the top, over everything else, as the QML's.
    let bar = chrome()
        .left(0.0)
        .right(0.0)
        .top(toolbar::TOP)
        .flex()
        .justify_center()
        .child(toolbar::toolbar(shared, root, buttons, controls));

    root_div
        .child(above)
        .child(below)
        .child(left)
        .child(right)
        .child(outline)
        .child(region_size)
        .child(lit)
        .child(title)
        .child(across)
        .child(down)
        .child(loupe)
        .child(bar)
        // A press on the snackbar is its own, as one on the toolbar is: its
        // events come here through the portal, and would let the region go
        // or pick the window under it.
        .child(
            chrome()
                .on_pointer_down(|ev| ev.stop_propagation())
                .on_pointer_up(|ev| ev.stop_propagation())
                .child(bars.view()),
        )
}

/// `s` covering `r`.
fn place(s: Style, r: &Rect) -> Style {
    s.left(r.x as f32)
        .top(r.y as f32)
        .w(r.width as f32)
        .h(r.height as f32)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::MAX_DELAY;
    use anti_webui::core::Ui;
    use anti_webui::core::event::{KeyEvent, Modifiers, WindowEvent};
    use anti_webui::core::geometry::{PointF, Scale, SizeI};
    use anti_webui::core::prelude::NodeRef;
    use serde_json::json;
    use std::cell::RefCell;

    /// Her scales: the ScreenPad's 1, eDP-1's 1.25 and the TV's 2.
    const SCALES: [f64; 3] = [1.0, 1.25, 2.0];

    /// An opaque picture of pseudo-random pixels, so that a shift or a blur
    /// by a pixel shows.
    fn fixture(width: u32, height: u32) -> Arc<Bitmap> {
        let mut seed = 0x2545_f491_4f6c_dd1du64;
        let mut pixels = Vec::with_capacity((width * height * 4) as usize);
        for _ in 0..width * height {
            seed = seed
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            let [r, g, b, ..] = (seed >> 24).to_le_bytes();
            pixels.extend_from_slice(&[r, g, b, 255]);
        }
        Arc::new(Bitmap::from_premultiplied(width, height, pixels, true))
    }

    /// The pixel of `frame` at `x, y`.
    fn pixel(frame: &Bitmap, x: u32, y: u32) -> [u8; 4] {
        let mut got = [0; 4];
        frame.with_levels(|level, (w, _), p| {
            if level == 0 {
                let at = ((y * w + x) * 4) as usize;
                got.copy_from_slice(&p[at..at + 4]);
            }
        });
        got
    }

    /// The windows of a `workspaces.get` view, for picking on eDP-1, which
    /// is 200x120 logical pixels at 1920,0 on the desktop. Each rect is the
    /// compositor's, with its 14 px ring.
    fn windows() -> Vec<WindowRect> {
        let window = |title: &str, rect: [f64; 4], focused_at: f64| {
            json!({
                "title": title, "minimized": false, "lastFocusedAt": focused_at,
                "rect": {"x": rect[0], "y": rect[1], "width": rect[2], "height": rect[3]},
            })
        };
        window::rects(&json!({
            "monitors": [
                {"name": "eDP-1", "workspaces": [{"active": true, "windows": [
                    // 20,20 to 120,80 on eDP-1, once its ring is off.
                    window("kitty", [1926.0, 6.0, 128.0, 88.0], 1000.0),
                    // Over kitty's right side: 90,40 to 150,100.
                    window("floorp", [1996.0, 26.0, 88.0, 88.0], 3000.0),
                    // Hanging off the right and the bottom: 170,90 on.
                    window("off the edge", [2076.0, 76.0, 128.0, 128.0], 500.0),
                    // Along the bottom of a WIDE output, under its
                    // snackbar: 0,180 to 800,300.
                    window("low", [1906.0, 166.0, 828.0, 148.0], 100.0),
                ]}]},
                // Under eDP-1's 20,20 on the desktop, but DP-1's.
                {"name": "DP-1", "workspaces": [{"active": true, "windows": [
                    window("other output", [1926.0, 6.0, 128.0, 88.0], 9000.0),
                ]}]},
            ],
        }))
    }

    fn rect(x: f64, y: f64, width: f64, height: f64) -> Rect {
        Rect {
            x,
            y,
            width,
            height,
        }
    }

    /// What the daemon keeps for the overlays: picking off, no delay, no
    /// failure, unless told.
    fn shared(pick: bool, delay: u8) -> Shared {
        Shared {
            pick: signal(pick),
            delay: signal(delay),
            failure: signal(None),
        }
    }

    /// An overlay of eDP-1 in a `Ui`, 200x120 logical pixels at `scale`
    /// unless told, open as a layer would be, and what it tells and gives.
    struct Open {
        ui: Ui,
        scale: f64,
        heard: Rc<RefCell<Vec<Told>>>,
        region: Rc<Cell<Option<Rect>>>,
        shared: Shared,
        controls: Controls,
        frame: Arc<Bitmap>,
    }

    /// Its toolbar spans all of it from 28 to 72 down, and takes the
    /// presses there: a press for the frame is above or below that.
    const LOGICAL: (f64, f64) = (200.0, 120.0);
    /// Wide enough for the toolbar.
    const WIDE: (f64, f64) = (800.0, 300.0);

    impl Open {
        fn new(scale: f64) -> Open {
            Open::sized(scale, LOGICAL)
        }

        fn sized(scale: f64, logical: (f64, f64)) -> Open {
            let mut open = Open::unfocused(scale, logical);
            settle(&mut open.ui);
            open
        }

        /// Before its first frame, which is when it focuses itself.
        fn unfocused(scale: f64, logical: (f64, f64)) -> Open {
            let heard: Rc<RefCell<Vec<Told>>> = Rc::default();
            let region: Rc<Cell<Option<Rect>>> = Rc::default();
            let shared = shared(false, 0);
            let controls = Controls::new();
            let size = ((logical.0 * scale) as u32, (logical.1 * scale) as u32);
            let frame = fixture(size.0, size.1);
            let (tell, out, shown): (Rc<dyn Fn(Told)>, _, _) = {
                let heard = heard.clone();
                (
                    Rc::new(move |t| heard.borrow_mut().push(t)),
                    region.clone(),
                    frame.clone(),
                )
            };
            let mut ui = Ui::new(move || {
                theme::install();
                let frozen = Frozen {
                    output: "eDP-1".into(),
                    position: (1920.0, 0.0),
                    frame: shown,
                    windows: windows(),
                };
                view_with(frozen, shared, out, tell, controls)
            });
            ui.handle_event(WindowEvent::Resized {
                size: SizeI::new(size.0 as i32, size.1 as i32),
                scale: Scale::from_f64(scale),
            });
            ui.handle_event(WindowEvent::Focused(true));
            ui.update();
            Open {
                ui,
                scale,
                heard,
                region,
                shared,
                controls,
                frame,
            }
        }

        /// A click in the middle of `node`, where the surface has it.
        fn click(&mut self, node: NodeRef) {
            let middle = self.onto(node);
            for pressed in [true, false] {
                self.primary(middle, pressed);
                self.ui.update();
            }
            settle(&mut self.ui);
        }

        /// A click on `node` whose press and release are handled together,
        /// with no frame between them or after: a tap, or a click while the
        /// overlay was busy drawing.
        fn tap(&mut self, node: NodeRef) {
            let middle = self.onto(node);
            self.primary(middle, true);
            self.primary(middle, false);
        }

        /// The pointer moved to the middle of `node`, which it gives.
        fn onto(&mut self, node: NodeRef) -> PointF {
            let r = node.rect().expect("a button that is shown");
            let middle = PointF::new(
                r.x as f32 + r.width as f32 / 2.0,
                r.y as f32 + r.height as f32 / 2.0,
            );
            self.ui.handle_event(WindowEvent::PointerMoved {
                position: middle,
                modifiers: Modifiers::default(),
            });
            self.ui.update();
            middle
        }

        /// The left button at `position`, in the surface's pixels.
        fn primary(&mut self, position: PointF, pressed: bool) {
            self.ui.handle_event(WindowEvent::PointerButton {
                position,
                button: PointerButton::Primary,
                pressed,
                modifiers: Modifiers::default(),
                time: 0,
            });
        }

        fn key(&mut self, key: NamedKey) {
            for pressed in [true, false] {
                self.ui.handle_event(WindowEvent::Key(KeyEvent {
                    key: Key::Named(key),
                    text: None,
                    modifiers: Modifiers::default(),
                    pressed,
                    repeat: false,
                }));
            }
            self.ui.update();
        }

        fn events(&self) -> Vec<Event> {
            self.heard
                .borrow_mut()
                .drain(..)
                .map(|t| match t {
                    Told::Event(e) => e,
                    other => panic!("{other:?}"),
                })
                .collect()
        }
    }

    /// The pointer on a `Ui`, in logical pixels: an [`Open`] overlay's, or
    /// one being rendered.
    trait Pointer {
        fn ui(&mut self) -> &mut Ui;
        fn scale(&self) -> f64;

        /// A point given in logical pixels, where the pointer's events have it.
        fn at(&self, x: f64, y: f64) -> PointF {
            let scale = self.scale();
            PointF::new((x * scale) as f32, (y * scale) as f32)
        }

        fn move_to(&mut self, x: f64, y: f64) {
            let position = self.at(x, y);
            let ui = self.ui();
            ui.handle_event(WindowEvent::PointerMoved {
                position,
                modifiers: Modifiers::default(),
            });
            ui.update();
        }

        fn button(&mut self, x: f64, y: f64, button: PointerButton, pressed: bool) {
            let position = self.at(x, y);
            let ui = self.ui();
            ui.handle_event(WindowEvent::PointerButton {
                position,
                button,
                pressed,
                modifiers: Modifiers::default(),
                time: 0,
            });
            ui.update();
        }

        fn press(&mut self, x: f64, y: f64) {
            self.button(x, y, PointerButton::Primary, true);
        }

        fn release(&mut self, x: f64, y: f64) {
            self.button(x, y, PointerButton::Primary, false);
        }

        /// A left-button drag from `a` to `b`, with a move half way.
        fn drag(&mut self, a: (f64, f64), b: (f64, f64)) {
            self.move_to(a.0, a.1);
            self.press(a.0, a.1);
            self.move_to((a.0 + b.0) / 2.0, (a.1 + b.1) / 2.0);
            self.move_to(b.0, b.1);
            self.release(b.0, b.1);
        }
    }

    impl Pointer for Open {
        fn ui(&mut self) -> &mut Ui {
            &mut self.ui
        }

        fn scale(&self) -> f64 {
            self.scale
        }
    }

    /// What a frame does before it is drawn: the frame callbacks, which
    /// focus the overlay, and the focus they ask for.
    fn settle(ui: &mut Ui) {
        ui.update();
        ui.animate(Instant::now());
        ui.update();
    }

    #[test]
    fn escape_cancels_and_enter_saves() {
        let mut o = Open::new(1.25);
        o.key(NamedKey::Escape);
        assert_eq!(o.events(), [Event::Cancel]);
        o.key(NamedKey::Enter);
        assert_eq!(o.events(), [Event::Save]);
        // Other keys are nobody's business here.
        o.key(NamedKey::Tab);
        o.key(NamedKey::Space);
        assert_eq!(o.events(), []);
    }

    #[test]
    fn the_pointer_coming_on_says_which_output_once() {
        let mut o = Open::new(1.0);
        o.move_to(5.0, 10.0);
        o.move_to(6.0, 10.0);
        o.move_to(7.0, 10.0);
        assert_eq!(o.events(), [Event::Pointer("eDP-1".into())]);
        // Gone and back: said again.
        o.ui.handle_event(WindowEvent::PointerLeft);
        o.ui.update();
        o.move_to(8.0, 10.0);
        assert_eq!(o.events(), [Event::Pointer("eDP-1".into())]);
    }

    /// The keyboard comes to an overlay when the pointer does, so its keys
    /// must reach it then: the pointer coming on focuses it, whatever had
    /// the focus before.
    #[test]
    fn the_pointer_coming_on_focuses_it() {
        for scale in SCALES {
            let mut o = Open::unfocused(scale, LOGICAL);
            o.key(NamedKey::Escape);
            assert_eq!(o.events(), [], "nothing focused yet");
            o.move_to(50.0, 50.0);
            assert_eq!(o.events(), [Event::Pointer("eDP-1".into())]);
            o.key(NamedKey::Escape);
            assert_eq!(o.events(), [Event::Cancel], "at {scale}");
        }
    }

    /// The pointer's events are in the surface's pixels; regions are in the
    /// output's logical pixels, whatever the scale.
    #[test]
    fn a_drag_is_a_region_in_logical_pixels_at_each_scale() {
        for scale in SCALES {
            let mut o = Open::new(scale);
            o.move_to(20.0, 76.0);
            o.press(20.0, 76.0);
            o.move_to(70.0, 96.0);
            // While it is dragged, the region follows.
            assert_eq!(
                o.region.get(),
                Some(rect(20.0, 76.0, 50.0, 20.0)),
                "{scale}"
            );
            o.move_to(120.0, 116.0);
            o.release(120.0, 116.0);
            assert_eq!(
                o.region.get(),
                Some(rect(20.0, 76.0, 100.0, 40.0)),
                "{scale}"
            );
            // Up and to the left is the same.
            o.drag((150.0, 100.0), (40.0, 10.0));
            assert_eq!(
                o.region.get(),
                Some(rect(40.0, 10.0, 110.0, 90.0)),
                "{scale}"
            );
            // Fractions of a logical pixel, as at 1.25, stay fractions: the
            // save rounds them as the QML did.
            o.drag((10.4, 10.8), (30.0, 40.0));
            let r = o.region.get().unwrap();
            assert!(
                (r.x - 10.4).abs() < 1e-5 && (r.y - 10.8).abs() < 1e-5,
                "{r:?}"
            );
            assert!((r.width - 19.6).abs() < 1e-5 && (r.height - 29.2).abs() < 1e-5);
        }
    }

    #[test]
    fn a_new_press_lets_the_old_region_go() {
        for scale in SCALES {
            let mut o = Open::new(scale);
            o.drag((20.0, 20.0), (100.0, 100.0));
            assert!(o.region.get().is_some());
            o.press(150.0, 90.0);
            assert_eq!(o.region.get(), None, "{scale}");
            // A click is not a region.
            o.release(150.0, 90.0);
            assert_eq!(o.region.get(), None, "{scale}");
        }
    }

    #[test]
    fn a_region_under_4_logical_pixels_either_way_is_let_go() {
        for scale in SCALES {
            let mut o = Open::new(scale);
            o.drag((10.0, 10.0), (14.0, 30.0));
            assert_eq!(o.region.get(), Some(rect(10.0, 10.0, 4.0, 20.0)), "{scale}");
            o.drag((10.0, 10.0), (13.5, 30.0));
            assert_eq!(o.region.get(), None, "{scale}");
            o.drag((10.0, 10.0), (60.0, 13.0));
            assert_eq!(o.region.get(), None, "{scale}");
            o.drag((10.0, 10.0), (60.0, 14.0));
            assert_eq!(o.region.get(), Some(rect(10.0, 10.0, 50.0, 4.0)), "{scale}");
        }
    }

    /// The surface keeps the moves and the release of a drag that leaves
    /// it: the region stays on the output.
    #[test]
    fn a_drag_past_the_edges_stays_on_the_output() {
        for scale in SCALES {
            let mut o = Open::new(scale);
            o.drag((150.0, 100.0), (260.0, 170.0));
            assert_eq!(
                o.region.get(),
                Some(rect(150.0, 100.0, 50.0, 20.0)),
                "{scale}"
            );
            o.drag((50.0, 80.0), (-30.0, -20.0));
            assert_eq!(o.region.get(), Some(rect(0.0, 0.0, 50.0, 80.0)), "{scale}");
            // Its crop is inside the frame, and at the far edges is the
            // frame's edge.
            o.drag((150.0, 100.0), (900.0, 900.0));
            let crop = crate::geometry::crop(
                o.region.get().as_ref(),
                (LOGICAL.0 as i32, LOGICAL.1 as i32),
                scale as f32,
                (o.frame.width(), o.frame.height()),
            )
            .unwrap();
            assert_eq!(crop.x + crop.width, o.frame.width(), "{scale}");
            assert_eq!(crop.y + crop.height, o.frame.height(), "{scale}");
        }
    }

    #[test]
    fn only_the_left_button_makes_a_region() {
        let mut o = Open::new(2.0);
        o.move_to(20.0, 20.0);
        for button in [PointerButton::Secondary, PointerButton::Middle] {
            o.button(20.0, 20.0, button, true);
            o.move_to(100.0, 100.0);
            o.button(100.0, 100.0, button, false);
            assert_eq!(o.region.get(), None, "{button:?}");
        }
        // A right-click no longer cancels: only the pointer coming on was said.
        assert_eq!(o.events(), [Event::Pointer("eDP-1".into())]);
        // Nor does it let a region go.
        o.drag((20.0, 20.0), (100.0, 100.0));
        o.button(50.0, 50.0, PointerButton::Secondary, true);
        o.button(50.0, 50.0, PointerButton::Secondary, false);
        assert_eq!(o.region.get(), Some(rect(20.0, 20.0, 80.0, 80.0)));
    }

    /// In pick mode a press on a window makes its rect the region, on this
    /// output and in its logical pixels; on empty desktop it does nothing.
    #[test]
    fn a_pick_makes_the_topmost_window_the_region() {
        for scale in SCALES {
            let mut o = Open::new(scale);
            o.drag((5.0, 5.0), (15.0, 15.0));
            o.events();
            o.shared.pick.set(true);
            // Empty desktop: no region taken, the old one kept, still picking.
            o.press(180.0, 10.0);
            o.release(180.0, 10.0);
            assert_eq!(o.region.get(), Some(rect(5.0, 5.0, 10.0, 10.0)), "{scale}");
            assert_eq!(o.events(), []);
            // kitty alone.
            o.press(30.0, 76.0);
            o.release(30.0, 76.0);
            assert_eq!(
                o.region.get(),
                Some(rect(20.0, 20.0, 100.0, 60.0)),
                "{scale}"
            );
            assert_eq!(o.events(), [Event::Picked]);
            // Where floorp is over kitty, floorp: it had the keyboard last.
            o.press(100.0, 76.0);
            o.release(100.0, 76.0);
            assert_eq!(
                o.region.get(),
                Some(rect(90.0, 40.0, 60.0, 60.0)),
                "{scale}"
            );
            // Hanging off the output: only what is on it.
            o.press(190.0, 115.0);
            o.release(190.0, 115.0);
            assert_eq!(
                o.region.get(),
                Some(rect(170.0, 90.0, 30.0, 30.0)),
                "{scale}"
            );
            // kitty, not DP-1's window at the same point of the desktop,
            // focused later but not this output's; and while picking, a
            // drag makes no region of its own.
            o.drag((25.0, 25.0), (60.0, 60.0));
            assert_eq!(
                o.region.get(),
                Some(rect(20.0, 20.0, 100.0, 60.0)),
                "{scale}"
            );
            // Picking over, a press drags again.
            o.shared.pick.set(false);
            o.drag((25.0, 25.0), (60.0, 60.0));
            assert_eq!(
                o.region.get(),
                Some(rect(25.0, 25.0, 35.0, 35.0)),
                "{scale}"
            );
        }
    }

    /// The toolbar's buttons say what they are for, and none of them, on or
    /// off, starts a region. After a click the keys are still the
    /// overlay's: Space and Tab do nothing, Enter saves and Esc lets go.
    #[test]
    fn the_toolbar_tells_drags_nothing_and_leaves_the_keys_alone() {
        for scale in SCALES {
            let mut o = Open::sized(scale, WIDE);
            o.move_to(400.0, 200.0);
            assert_eq!(o.events(), [Event::Pointer("eDP-1".into())]);
            let c = o.controls;
            o.click(c.more);
            assert_eq!(o.events(), [Event::Delay(1)], "{scale}");
            // The machine's answers, which the daemon gives every overlay.
            // At the ends the stepper's buttons are off, and say nothing.
            o.shared.delay.set(MAX_DELAY);
            settle(&mut o.ui);
            o.click(c.more);
            o.shared.delay.set(0);
            settle(&mut o.ui);
            o.click(c.less);
            assert_eq!(o.events(), [], "{scale}");
            o.shared.delay.set(5);
            settle(&mut o.ui);
            o.click(c.less);
            o.click(c.pick);
            o.click(c.retry);
            o.click(c.copy);
            assert_eq!(
                o.events(),
                [
                    Event::Delay(4),
                    Event::TogglePick,
                    Event::Retry,
                    Event::Copy
                ],
                "{scale}"
            );
            // Picking is the machine's to turn on; the toggle only asks.
            assert!(!o.shared.pick.get_untracked());
            assert_eq!(o.region.get(), None, "{scale}");
            // The keys, straight after a click: not the button's (Space
            // would copy again), but the overlay's.
            for key in [
                NamedKey::Space,
                NamedKey::Tab,
                NamedKey::Enter,
                NamedKey::Escape,
            ] {
                o.key(key);
            }
            assert_eq!(o.events(), [Event::Save, Event::Cancel], "{scale}");
            // A drag from a button that is off makes no region either.
            o.shared.delay.set(0);
            settle(&mut o.ui);
            let r = c.less.rect().unwrap();
            let from = (
                (r.x as f64 + r.width as f64 / 2.0) / scale,
                (r.y as f64 + r.height as f64 / 2.0) / scale,
            );
            o.drag(from, (500.0, 250.0));
            assert_eq!(o.region.get(), None, "{scale}");
            assert_eq!(o.events(), [], "{scale}");
            // And a drag on the frame makes a region as before.
            o.drag((100.0, 150.0), (300.0, 250.0));
            assert_eq!(
                o.region.get(),
                Some(rect(100.0, 150.0, 200.0, 100.0)),
                "{scale}"
            );
        }
    }

    /// Frames for `ms` milliseconds, for what moves in.
    fn frames(ui: &mut Ui, ms: u64) {
        let start = Instant::now();
        for t in (0..=ms).step_by(16) {
            ui.animate(start + std::time::Duration::from_millis(t));
            ui.update();
        }
    }

    /// Whether a box of `background` is painted, in a layer or not.
    fn painted(ui: &mut Ui, background: anti_webui::prelude::Color) -> bool {
        use anti_webui::core::scene::{Primitive, Scene};
        fn has(scene: &Scene, background: anti_webui::prelude::Color) -> bool {
            scene.primitives.iter().any(|p| match p {
                Primitive::Quad(q) => q.background == background,
                Primitive::Layer(l) => has(&l.content, background),
                _ => false,
            })
        }
        has(&ui.paint(Instant::now()), background.snapped())
    }

    /// A failure is said on the overlay of the output it names, md3e's
    /// snackbar in the inverse colours, and the keys stay the overlay's.
    #[test]
    fn a_failure_is_said_on_its_own_output() {
        let mut o = Open::sized(1.25, WIDE);
        let snackbar = color(crate::theme::TEXT);
        o.shared.failure.set(Some(failure("DP-1")));
        frames(&mut o.ui, 300);
        assert!(!painted(&mut o.ui, snackbar), "said on another output");
        o.shared.failure.set(Some(failure("eDP-1")));
        frames(&mut o.ui, 300);
        assert!(painted(&mut o.ui, snackbar), "said here");
        o.key(NamedKey::Enter);
        o.key(NamedKey::Escape);
        assert_eq!(o.events(), [Event::Save, Event::Cancel]);
    }

    /// A failed save, said on eDP-1.
    fn failure(output: &str) -> Failure {
        Failure {
            output: output.into(),
            text: "Not saved: cannot write it: No space left on device".into(),
        }
    }

    /// A press on the snackbar is the snackbar's: the region it is there to
    /// keep stays, the window under it is not picked, and the keys are
    /// still the overlay's.
    #[test]
    fn a_press_on_the_snackbar_keeps_the_region() {
        // The middle of md3e's snackbar: 48 px high, 24 px up from the
        // bottom, in the middle. The window "low" is under it.
        let on_it = (WIDE.0 / 2.0, WIDE.1 - 48.0);
        for scale in SCALES {
            // With no snackbar there, a press there picks "low".
            let mut o = Open::sized(scale, WIDE);
            o.shared.pick.set(true);
            o.move_to(on_it.0, on_it.1);
            o.press(on_it.0, on_it.1);
            o.release(on_it.0, on_it.1);
            assert_eq!(o.region.get(), Some(rect(0.0, 180.0, 800.0, 120.0)));
            assert_eq!(o.events(), [Event::Pointer("eDP-1".into()), Event::Picked]);
            for pick in [false, true] {
                let mut o = Open::sized(scale, WIDE);
                // Below the toolbar, above the snackbar.
                o.drag((100.0, 90.0), (300.0, 160.0));
                let region = Some(rect(100.0, 90.0, 200.0, 70.0));
                assert_eq!(o.region.get(), region);
                o.shared.pick.set(pick);
                o.shared.failure.set(Some(failure("eDP-1")));
                frames(&mut o.ui, 300);
                o.events();
                o.move_to(on_it.0, on_it.1);
                o.press(on_it.0, on_it.1);
                o.release(on_it.0, on_it.1);
                settle(&mut o.ui);
                assert_eq!(o.region.get(), region, "at {scale}, picking {pick}");
                o.key(NamedKey::Escape);
                assert_eq!(o.events(), [Event::Cancel], "at {scale}, picking {pick}");
            }
        }
    }

    /// A stepper button turned off while it has the focus leaves the keys
    /// the overlay's, also when the press and the release of the click came
    /// together, before the overlay could take the focus back: the machine's
    /// answer turns it off first.
    #[test]
    fn a_stepper_button_turned_off_under_a_tap_leaves_the_keys_alone() {
        for scale in SCALES {
            for (from, to) in [(1, 0), (MAX_DELAY - 1, MAX_DELAY)] {
                let mut o = Open::sized(scale, WIDE);
                o.move_to(400.0, 200.0);
                o.shared.delay.set(from);
                settle(&mut o.ui);
                let button = if to < from {
                    o.controls.less
                } else {
                    o.controls.more
                };
                o.tap(button);
                let said = [Event::Pointer("eDP-1".into()), Event::Delay(to.into())];
                assert_eq!(o.events(), said, "at {scale}");
                o.shared.delay.set(to);
                settle(&mut o.ui);
                o.key(NamedKey::Escape);
                o.key(NamedKey::Enter);
                assert_eq!(
                    o.events(),
                    [Event::Cancel, Event::Save],
                    "{from} to {to} at {scale}"
                );
            }
        }
    }

    #[test]
    fn a_spot_is_logical_and_physical_at_each_scale() {
        let s = Spot::new((101.0, 50.0), (1920.0, 1080.0), 1.25);
        assert_eq!((s.x, s.y), (80.8, 40.0));
        assert_eq!((s.width, s.height), (1536.0, 864.0));
        let s = Spot::new((101.0, 50.0), (3840.0, 2160.0), 2.0);
        assert_eq!((s.x, s.y, s.width, s.height), (50.5, 25.0, 1920.0, 1080.0));
        let s = Spot::new((101.0, 50.0), (1920.0, 515.0), 1.0);
        assert_eq!((s.x, s.y, s.width, s.height), (101.0, 50.0, 1920.0, 515.0));
    }

    /// The cells of the loupe, a pixel each, in `[r, g, b, a]`.
    fn cells_of(bitmap: &Bitmap) -> Vec<[u8; 4]> {
        let mut cells = Vec::new();
        bitmap.with_levels(|level, _, p| {
            if level == 0 {
                cells = p.as_chunks::<4>().0.to_vec();
            }
        });
        cells
    }

    /// The middle cell is the frame's pixel under the pointer, and the
    /// others are a logical pixel apart, as in the QML.
    #[test]
    fn the_loupe_is_the_frame_round_the_pointer_a_logical_pixel_a_cell() {
        let n = LOUPE_CELLS;
        for scale in SCALES {
            let (w, h) = (200.0 * scale, 120.0 * scale);
            let frame = fixture(w as u32, h as u32);
            // Not on a pixel's corner, as a pointer seldom is.
            let spot = Spot::new((101.3, 57.8), (w, h), scale);
            let cells = cells_of(&loupe_cells(&frame, &spot).unwrap());
            assert_eq!(cells.len(), (n * n) as usize);
            let cell = |col: u32, row: u32| cells[(row * n + col) as usize];
            assert_eq!(cell(10, 10), pixel(&frame, 101, 57), "at {scale}");
            for (col, row) in [(0, 0), (20, 20), (13, 4), (10, 0)] {
                let x = (101.3 + (col as f64 - 10.0) * scale).floor() as u32;
                let y = (57.8 + (row as f64 - 10.0) * scale).floor() as u32;
                assert_eq!(
                    cell(col, row),
                    pixel(&frame, x, y),
                    "{col},{row} at {scale}"
                );
            }
            // In a corner, what is off the frame is clear.
            let corner = Spot::new((1.0, 1.0), (w, h), scale);
            let cells = cells_of(&loupe_cells(&frame, &corner).unwrap());
            assert_eq!(cells[0], [0; 4]);
            assert_eq!(cells[(10 * n + 10) as usize], pixel(&frame, 1, 1));
        }
    }

    #[test]
    fn the_loupe_goes_below_right_and_flips_at_the_edges() {
        let spot = |x: f64, y: f64| Spot::new((x, y), (1536.0, 864.0), 1.0);
        assert_eq!(loupe_at(&spot(100.0, 100.0)), (124.0, 124.0));
        // 128 wide and 146 high, 24 away: flipped where it would leave.
        assert_eq!(loupe_at(&spot(1384.0, 694.0)), (1408.0, 718.0));
        assert_eq!(
            loupe_at(&spot(1385.0, 695.0)),
            (1385.0 - 152.0, 695.0 - 170.0)
        );
        assert_eq!(loupe_at(&spot(1500.0, 10.0)), (1348.0, 34.0));
    }

    #[test]
    fn the_loupe_reads_the_pixel_and_the_regions_size() {
        let spot = Spot::new((101.0, 50.0), (1920.0, 1080.0), 1.25);
        assert_eq!(readout(&spot, None), "80, 40");
        assert_eq!(
            readout(&spot, Some(rect(1.0, 2.0, 299.5, 40.4))),
            "80, 40  ·  300×40"
        );
    }

    /// Renders `view` of eDP-1 as a layer would show it, with `drive` given
    /// the logical-pixel pointer helpers: software only, as the comment on
    /// the pixel test says.
    fn render(
        logical: (f64, f64),
        scale: f64,
        frame: Arc<Bitmap>,
        windows: Vec<WindowRect>,
        shared: Shared,
        drive: impl FnOnce(&mut Driver),
    ) -> Vec<u8> {
        let size = ((logical.0 * scale) as u32, (logical.1 * scale) as u32);
        anti_webui::app::headless::render_driven_rgba(
            size.0,
            size.1,
            scale,
            move || {
                theme::install();
                let frozen = Frozen {
                    output: "eDP-1".into(),
                    position: (0.0, 0.0),
                    frame,
                    windows,
                };
                view(frozen, shared, Rc::default(), Rc::new(|_| {}))
            },
            move |ui| {
                // Focused, as it is when it opens: no ring may show.
                settle(ui);
                drive(&mut Driver { ui, scale });
            },
        )
        .expect("a headless render")
    }

    /// The pointer on a `Ui` being rendered, in logical pixels.
    struct Driver<'a> {
        ui: &'a mut Ui,
        scale: f64,
    }

    impl Pointer for Driver<'_> {
        fn ui(&mut self) -> &mut Ui {
            self.ui
        }

        fn scale(&self) -> f64 {
            self.scale
        }
    }

    impl Driver<'_> {
        /// What moves, moved on by `ms` milliseconds.
        fn wait(&mut self, ms: u64) {
            frames(self.ui, ms);
        }
    }

    /// The frame's pixels, as the save reads them.
    fn pixels_of(frame: &Bitmap) -> Vec<u8> {
        let mut pixels = Vec::new();
        frame.with_levels(|level, _, p| {
            if level == 0 {
                pixels = p.to_vec();
            }
        });
        pixels
    }

    /// The frame on screen is the frame captured, pixel for pixel, at each
    /// of her scales: the ScreenPad's 1, eDP-1's 1.25 and the TV's 2. A
    /// shift or a softening at the freeze would be a difference here.
    ///
    /// Each render makes a GPU device of its own, and on her Intel GPU the
    /// first job of a new one sometimes faults, so this and the other
    /// renders are left out of `cargo test` and run alone, in Mesa's
    /// software renderer:
    ///
    /// ```sh
    /// env -u WAYLAND_DISPLAY WGPU_BACKEND=gl LIBGL_ALWAYS_SOFTWARE=1 \
    ///     __EGL_VENDOR_LIBRARY_FILENAMES=/usr/share/glvnd/egl_vendor.d/50_mesa.json \
    ///     cargo test -p minkashot -- --ignored --nocapture overlay::tests::render
    /// ```
    #[test]
    #[ignore = "renders on the GPU: run alone, as the comment says"]
    fn render_the_frame_shows_pixel_for_pixel_at_each_scale() {
        // Logical pixels in whole numbers, as an output's are, and wide
        // enough for the toolbar.
        let logical = (800.0, 300.0);
        for scale in SCALES {
            let (width, height) = ((logical.0 * scale) as u32, (logical.1 * scale) as u32);
            let frame = fixture(width, height);
            let want = pixels_of(&frame);
            // The pointer has not come on: no crosshair, no loupe. The
            // toolbar's band at the top is the toolbar's, a row more each
            // side for its edges; every other pixel is the frame's.
            let got = render(logical, scale, frame, Vec::new(), shared(false, 0), |_| {});
            assert_eq!(got.len(), want.len());
            let band = ((toolbar::TOP as f64 - 1.0) * scale) as u32
                ..((toolbar::TOP as f64 + 46.0) * scale).ceil() as u32;
            let wrong: Vec<usize> = (0..want.len() / 4)
                .filter(|&i| !band.contains(&(i as u32 / width)))
                .filter(|&i| got[i * 4..i * 4 + 4] != want[i * 4..i * 4 + 4])
                .collect();
            if let Some(&first) = wrong.first() {
                let (x, y) = (first as u32 % width, first as u32 / width);
                panic!(
                    "at scale {scale}, {} of {} pixels differ; the first at {x},{y}: \
                     {:?} shown, {:?} captured",
                    wrong.len(),
                    want.len() / 4,
                    &got[first * 4..first * 4 + 4],
                    &want[first * 4..first * 4 + 4]
                );
            }
            // The band has the toolbar in it, and the frame round it.
            let toolbar_drawn = band
                .clone()
                .flat_map(|y| (0..width).map(move |x| ((y * width + x) * 4) as usize))
                .filter(|&at| got[at..at + 4] != want[at..at + 4])
                .count();
            assert!(toolbar_drawn > 1000, "{toolbar_drawn} pixels of toolbar");
            eprintln!(
                "scale {scale}: {width}x{height}, every pixel the frame's but the toolbar's band"
            );
        }
    }

    /// What the loupe shows in its middle cell is the pixel under the
    /// pointer, as drawn, at each scale. Run as the comment above says.
    #[test]
    #[ignore = "renders on the GPU: run alone, as the comment on the pixel test says"]
    fn render_the_loupes_middle_is_the_pixel_under_the_pointer() {
        let logical = (400.0, 300.0);
        for scale in SCALES {
            let (width, height) = ((logical.0 * scale) as u32, (logical.1 * scale) as u32);
            let frame = fixture(width, height);
            // On a pixel, and between pixels.
            for (x, y) in [(101.0, 57.0), (101.4, 57.8)] {
                let got = render(
                    logical,
                    scale,
                    frame.clone(),
                    Vec::new(),
                    shared(false, 0),
                    |d| d.move_to(x, y),
                );
                // Where the event has it: in the surface's pixels, as f32.
                let physical = (((x * scale) as f32) as f64, ((y * scale) as f32) as f64);
                let spot = Spot::new(physical, (width as f64, height as f64), scale);
                let under = (physical.0.floor() as u32, physical.1.floor() as u32);
                // The middle of the middle cell, in the surface's pixels:
                // the loupe's corner, its border, then half its cells.
                let (lx, ly) = loupe_at(&spot);
                let border = Scale::from_f64(scale).px_min1(1.0) as u32;
                let half = Scale::from_f64(scale).px(LOUPE_BODY) as u32 / 2;
                let corner = |v: f64| Scale::from_f64(scale).px(v as f32) as u32;
                let (mx, my) = (corner(lx) + border + half, corner(ly) + border + half);
                let at = ((my * width + mx) * 4) as usize;
                assert_eq!(
                    got[at..at + 4],
                    pixel(&frame, under.0, under.1),
                    "pointer at {x},{y} (pixel {under:?}) at {scale}: the loupe's middle at {mx},{my}"
                );
                eprintln!(
                    "scale {scale}, pointer at {x},{y}: the loupe's middle is pixel {under:?}"
                );
            }
        }
    }

    /// A made-up desktop, `width` by `height` pixels: a grid on a dark
    /// ground, and `windows` (in pixels) with a title bar, lines of
    /// word-like dashes for text, a block of coloured cells and a fine
    /// checkerboard, so the loupe has single pixels to show. No real screen.
    fn desktop(width: u32, height: u32, windows: &[[u32; 4]]) -> Arc<Bitmap> {
        use std::f32::consts::TAU;
        let mut px = vec![0u8; (width * height * 4) as usize];
        let mut put = |x: u32, y: u32, c: [u8; 3]| {
            if x < width && y < height {
                let at = ((y * width + x) * 4) as usize;
                px[at..at + 4].copy_from_slice(&[c[0], c[1], c[2], 255]);
            }
        };
        for y in 0..height {
            for x in 0..width {
                let t = (x + y) as f32 / (width + height) as f32;
                let mut c = [(20.0 + 40.0 * t) as u8, 14, (36.0 + 50.0 * (1.0 - t)) as u8];
                if x % 40 == 0 || y % 40 == 0 {
                    c = [60, 48, 72];
                }
                put(x, y, c);
            }
        }
        let mut seed = 7u64;
        let mut next = |n: u32| {
            seed = seed
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            ((seed >> 33) % n as u64) as u32
        };
        for (i, &[wx, wy, ww, wh]) in windows.iter().enumerate() {
            for y in wy..wy + wh {
                for x in wx..wx + ww {
                    let edge = x == wx || y == wy || x == wx + ww - 1 || y == wy + wh - 1;
                    let bar = y < wy + 22;
                    let c = match (edge, bar, i) {
                        (true, ..) => [140, 30, 40],
                        (_, true, _) => [46, 34, 40],
                        (_, _, 0) => [236, 229, 231],
                        _ => [22, 16, 19],
                    };
                    put(x, y, c);
                }
            }
            let ink = if i == 0 {
                [40, 36, 38]
            } else {
                [200, 190, 195]
            };
            // Lines of words.
            let mut y = wy + 32;
            while y + 4 < wy + wh * 2 / 3 {
                let mut x = wx + 12;
                while x + 50 < wx + ww - 12 {
                    let word = 6 + next(30);
                    for dy in 0..5 {
                        for dx in 0..word {
                            if (dx + dy) % 7 != 3 {
                                put(x + dx, y + dy, ink);
                            }
                        }
                    }
                    x += word + 5;
                }
                y += 11;
            }
            // Coloured cells, and a checkerboard of single pixels.
            let top = wy + wh * 2 / 3 + 6;
            for cell in 0..12u32 {
                let hue = cell as f32 / 12.0;
                let c = [
                    (255.0 * (0.5 + 0.5 * (TAU * hue).cos())) as u8,
                    (255.0 * (0.5 + 0.5 * (TAU * (hue - 0.333)).cos())) as u8,
                    (255.0 * (0.5 + 0.5 * (TAU * (hue - 0.667)).cos())) as u8,
                ];
                for y in top..(top + 18).min(wy + wh - 2) {
                    for x in wx + 12 + cell * 20..wx + 30 + cell * 20 {
                        put(x, y, c);
                    }
                }
            }
            for y in top + 24..wy + wh - 4 {
                for x in wx + 12..wx + 72 {
                    put(
                        x,
                        y,
                        if (x + y) % 2 == 0 {
                            [255, 255, 255]
                        } else {
                            [0, 0, 0]
                        },
                    );
                }
            }
        }
        Arc::new(Bitmap::from_premultiplied(width, height, px, true))
    }

    /// The overlay's looks, for her to see: PNGs of a made-up desktop at
    /// eDP-1's scale, 1.25, in the states the QML had and the toolbar's,
    /// the countdown's, and the toolbar in both densities, written to the
    /// directory `MINKASHOT_SHOTS` names. Run as the comment on the pixel
    /// test says, with `MINKASHOT_SHOTS=<dir>` added.
    #[test]
    #[ignore = "renders on the GPU: run alone, as the comment on the pixel test says"]
    fn render_the_states_to_look_at() {
        let dir = std::path::PathBuf::from(
            std::env::var_os("MINKASHOT_SHOTS").expect("MINKASHOT_SHOTS names where they go"),
        );
        std::fs::create_dir_all(&dir).unwrap();
        let scale = 1.25;
        let logical = (768.0, 432.0);
        let size = ((logical.0 * scale) as u32, (logical.1 * scale) as u32);
        // Two windows, in logical pixels, and in the frame's.
        let shown = [[64.0, 48.0, 320.0, 208.0], [300.0, 160.0, 360.0, 216.0]];
        let in_frame: Vec<[u32; 4]> = shown
            .iter()
            .map(|r| r.map(|v| (v * scale) as u32))
            .collect();
        let frame = desktop(size.0, size.1, &in_frame);
        // As workspaces.get gives them: with the 14 px ring, the one in
        // front focused last.
        let rect = |r: [f64; 4]| json!({"x": r[0] - 14.0, "y": r[1] - 14.0, "width": r[2] + 28.0, "height": r[3] + 28.0});
        let windows = window::rects(&json!({"monitors": [{"name": "eDP-1", "workspaces": [
            {"active": true, "windows": [
                {"title": "kitty: fish ~/Projects", "minimized": false,
                 "lastFocusedAt": 1000, "rect": rect(shown[0])},
                {"title": "Floorp: MinkaDE notes", "minimized": false,
                 "lastFocusedAt": 2000, "rect": rect(shown[1])},
            ]},
        ]}]}));
        let write = |name: &str, size: (u32, u32), rgba: &[u8]| {
            let png = anti_webui::app::headless::encode_png(size.0, size.1, rgba).unwrap();
            let path = dir.join(format!("{name}.png"));
            std::fs::write(&path, png).unwrap();
            eprintln!("{}", path.display());
        };

        // The overlay: picking or not, the delay, a failure to say, and
        // where the pointer goes.
        type Drive = fn(&mut Driver);
        let full = "Not saved: cannot write ~/Pictures/Screenshots/minkashot-20261125-142233.png: \
                    No space left on device (os error 28)";
        let states: [(&str, bool, u8, Option<&str>, Drive); 9] = [
            ("1-idle-crosshair-loupe", false, 0, None, |d| {
                d.move_to(200.3, 120.6)
            }),
            ("2-region-scrim-outline", false, 0, None, |d| {
                d.drag((100.0, 80.0), (420.4, 300.2))
            }),
            ("3-loupe-flipped-bottom-right", false, 0, None, |d| {
                d.drag((470.0, 250.0), (640.0, 352.0));
                d.move_to(735.0, 405.0);
            }),
            ("4-pick-hovering-a-window", true, 0, None, |d| {
                d.move_to(520.0, 300.0)
            }),
            ("5-toolbar-delay-0", false, 0, None, |_| {}),
            ("6-toolbar-delay-5", false, 5, None, |_| {}),
            ("7-toolbar-delay-10", false, 10, None, |_| {}),
            ("8-toolbar-pick-on", true, 0, None, |d| {
                d.move_to(150.0, 200.0)
            }),
            ("9-failure-snackbar", false, 3, Some(full), |d| {
                d.drag((120.0, 110.0), (360.0, 280.0));
                d.move_to(500.0, 230.0);
                d.wait(500);
            }),
        ];
        for (name, pick, delay, failure, drive) in states {
            let shared = shared(pick, delay);
            shared.failure.set(failure.map(|text| Failure {
                output: "eDP-1".into(),
                text: text.into(),
            }));
            let rgba = render(
                logical,
                scale,
                frame.clone(),
                windows.clone(),
                shared,
                drive,
            );
            write(name, size, &rgba);
        }

        // The countdown before a retry of 3 s, at 3, 2 and 1: each second
        // a step down, which the ring takes half a second to make.
        for at in [3u8, 2, 1] {
            let left = signal(3u8);
            let frame = frame.clone();
            let rgba = anti_webui::app::headless::render_driven_rgba(
                size.0,
                size.1,
                scale,
                move || {
                    theme::install();
                    div()
                        .size_full()
                        .relative()
                        .child(bitmap_view(move || Some(frame.clone())).size_full())
                        .child(
                            chrome()
                                .left(0.0)
                                .right(0.0)
                                .top(crate::countdown::TOP as f32)
                                .flex()
                                .justify_center()
                                .child(crate::countdown::view(left, 3)),
                        )
                },
                move |ui| {
                    settle(ui);
                    for step in (at..3).rev() {
                        left.set(step);
                        frames(ui, 1000);
                    }
                },
            )
            .expect("a headless render");
            write(&format!("10-countdown-{at}"), size, &rgba);
        }

        // The toolbar as md3e builds it by default, and compact, as here.
        let bar = (720u32, 180u32);
        let rgba = anti_webui::app::headless::render_rgba(
            (bar.0 as f64 * scale) as u32,
            (bar.1 as f64 * scale) as u32,
            scale,
            move || {
                use anti_webui_md3e::{Density, set_density};
                proustite::install();
                let one = |density| {
                    set_density(density);
                    toolbar::toolbar(
                        shared(false, 3),
                        node_ref(),
                        Rc::new(|_| {}),
                        Controls::new(),
                    )
                };
                let (standard, compact) = (one(Density::Standard), one(Density::Compact));
                div()
                    .size_full()
                    .bg(color(GROUND))
                    .flex()
                    .flex_col()
                    .items_center()
                    .justify_center()
                    .gap(24.0_f32)
                    .child(standard)
                    .child(compact)
            },
        )
        .expect("a headless render");
        let bar = ((bar.0 as f64 * scale) as u32, (bar.1 as f64 * scale) as u32);
        write("11-toolbar-standard-then-compact", bar, &rgba);
    }
}
