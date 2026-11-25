//! The countdown before a retry, as the QML CountdownChip was: a pill near
//! the top of the active output with the seconds left, here in md3e's wavy
//! ring, which goes down a step each second. It is on a layer of its own
//! that every click goes through (`Daemon::countdown`), closed, and the
//! screen given time to be drawn without it, before the capture it counts
//! down to.

use anti_webui::core::prelude::{Div, Signal, div};
use anti_webui::core::style::Styled;
use anti_webui_md3e::components::circular_wavy_progress;

use crate::theme::{FONT_SIZE, RED, RED_DIM, SURFACE, color, mono_text};

/// How far down the output the pill is, in logical pixels.
pub const TOP: i32 = 120;
/// md3e's wavy ring, the pill's room round it, and its outline, the
/// QML pill's.
const RING: f32 = 48.0;
const PADDING: f32 = 6.0;
const BORDER: f32 = 1.0;

/// `left` seconds of `of`, the ring as full as what is left.
pub fn view(left: Signal<u8>, of: u8) -> Div {
    let of = of.max(1) as f32;
    let ring = circular_wavy_progress(move || left.get() as f32 / of);
    let seconds = div()
        .absolute()
        .left(0.0)
        .right(0.0)
        .top(0.0)
        .bottom(0.0)
        .flex()
        .items_center()
        .justify_center()
        .child(mono_text(
            move || left.get().to_string(),
            FONT_SIZE + 2.0,
            RED,
        ));
    div()
        .flex()
        .p(PADDING)
        .rounded(RING / 2.0 + PADDING + BORDER)
        .bg(color(SURFACE))
        .border(BORDER)
        .border_color(color(RED_DIM))
        .select_none()
        .child(div().relative().size(RING).child(ring).child(seconds))
}
