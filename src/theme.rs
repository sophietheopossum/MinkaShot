//! Proustite (the `proustite` crate, generated from Proustite.qml) and
//! MinkaShot's own tokens, the capture overlay's extras.

use anti_webui::prelude::Color;
use anti_webui_md3e::{Density, set_density};
pub use proustite::*;

/// Over the frozen frame outside the region: `ground` at 0.64.
pub fn scrim() -> Color {
    color_alpha(GROUND, 0.64)
}

/// The crosshair through the pointer: `red` at 0.35.
pub fn crosshair() -> Color {
    color_alpha(RED, 0.35)
}

/// The fill of the window a pick would take: `red` at 0.12.
pub fn pick_fill() -> Color {
    color_alpha(RED, 0.12)
}

/// How everything the daemon builds draws: the md3e components in their
/// compact sizes, and both widget sets in Proustite. Call before the first
/// view.
pub fn install() {
    set_density(Density::Compact);
    proustite::install();
}
