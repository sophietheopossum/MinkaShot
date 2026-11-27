//! MinkaShot, Minka's screenshot tool, on AntiWebUI.
//!
//! The command line: `minkashot full [<output>] [<path>]` and
//! `minkashot region <output> <x,y,w,h> [<path>]` capture an output, and
//! `minkashot win [<window>] [<path>]` one window's own picture, under
//! others or not (no raising, no focus taken), through the toolkit's
//! ext-image-copy-capture (shm, no pointer). They crop it, save a PNG
//! without ever leaving half a file under its name, print the path and exit
//! with a status that says what failed. A path of `-` sends the PNG to
//! standard output instead (for `wl-copy -t image/png`). They map no surface
//! and take no input, so a stuck one can never freeze the screen, and they
//! give up on a compositor that does not answer. `minkashot list` lists the
//! windows.
//!
//! The interactive overlay (Print) is `minkashot daemon` (`daemon.rs`). It
//! replaced the Quickshell daemon that was beside this crate until
//! 26/11/2026, which is "the QML" in these comments. The session starts it
//! through minka-keep (`packaging/minka-keep`), which logs it to
//! `~/.local/state/minka/minkashot.log` and restarts it if it dies. One per
//! session, it hears Print and answers `minkashot interactive|cancel|status`,
//! and steps through an arm (`state.rs`). An arm freezes every output under
//! an overlay showing its frame (`overlay.rs`), with the QML's crosshair,
//! loupe and toolbar
//! (`toolbar.rs`), where a drag or a picked window makes a region. Esc lets
//! go; Enter saves the region, or the output whole, and the toolbar's copy
//! puts it on the clipboard instead, the frames staying up, and saying why,
//! if either fails. The toolbar's retry freezes again after its delay,
//! counted down on a layer of its own (`countdown.rs`). With no arguments
//! this prints its usage, so nobody starts a daemon by accident.
//!
//! The picture and its PNG bytes are made apart from where they go
//! (`save.rs`), so the bytes can go to the clipboard instead.

mod cli;
mod countdown;
mod daemon;
mod geometry;
mod grab;
mod overlay;
mod save;
mod state;
mod theme;
mod toolbar;
mod window;

use std::process::ExitCode;

/// What MinkaDE's one binary runs as `minkashot`.
pub fn main() -> ExitCode {
    let args: Vec<std::ffi::OsString> = std::env::args_os().skip(1).collect();
    ExitCode::from(cli::run(&args))
}
