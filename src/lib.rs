//! MinkaShot, Minka's screenshot tool, on AntiWebUI.
//!
//! So far the command line: `minkashot full [<output>] [<path>]` and
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
//! MinkaCap, the window capture this replaces, is a name of this too, until
//! the QML daemon that runs it is gone: see [`cap_main`].
//!
//! The interactive overlay (Print) is still the QML tree beside this crate,
//! which the session runs with `qs`. `minkashot daemon` is the Rust daemon
//! that is to replace it (`daemon.rs`): one per session, it hears Print and
//! answers `minkashot interactive|cancel|status`, and steps through an arm
//! (`state.rs`). An arm freezes every output under an overlay showing its
//! frame (`overlay.rs`), with the QML's crosshair, loupe and toolbar
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

/// What MinkaDE's one binary runs as `MinkaCap`, the window capture that
/// was its own crate: `MinkaCap grab <window> <path>` is `minkashot win`,
/// and `MinkaCap list` is `minkashot list`, with minkashot's exit statuses.
/// The QML MinkaShot daemon runs it for its window shots
/// (`${MINKA_CAP_BIN:-/usr/bin/MinkaCap} grab <title> <path>`), so it goes
/// with that.
pub fn cap_main() -> ExitCode {
    let mut args: Vec<std::ffi::OsString> = std::env::args_os().skip(1).collect();
    match args.first().and_then(|a| a.to_str()) {
        Some("grab") if args.len() == 3 => args[0] = "win".into(),
        Some("list") if args.len() == 1 => {}
        _ => {
            minka_note::note!(
                "MinkaCap: usage: MinkaCap list | grab <window> <path> \
                 (which are minkashot list and minkashot win <window> [<path>])"
            );
            return ExitCode::from(2);
        }
    }
    ExitCode::from(cli::run(&args))
}
