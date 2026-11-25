//! Picking the output, and taking one picture of it or of a window with the
//! toolkit's capture (ext-image-copy-capture into shm: no GPU, no surface,
//! no input).

use anti_webui::core::images::Bitmap;
use anti_webui::shell::{CaptureOptions, CaptureTarget, Output, capture};
use std::sync::Arc;
use std::sync::mpsc::{self, RecvTimeoutError};
use std::time::{Duration, Instant};

use crate::cli::Failure;

/// How long a picture may take, as the QML's watchdog allowed a scripted
/// shot.
pub const PICTURE_TIMEOUT: Duration = Duration::from_secs(6);

/// How long the compositor may take to say which output is focused.
const FOCUS_TIMEOUT: Duration = Duration::from_secs(1);

/// How long everything that talks to the compositor may take in all: the
/// picture, the focused output or a window's role and the window list, and
/// reaching it at all, which has no bound of its own. A compositor that
/// takes the connection and then answers nothing would otherwise hold the
/// command for ever.
pub const SHOT_TIMEOUT: Duration = Duration::from_secs(9);

/// Runs `f` on a thread of its own and waits at most `timeout` for it. A
/// thread still stuck then is left behind; the command is about to exit,
/// which ends it.
pub fn within<T: Send + 'static>(
    timeout: Duration,
    f: impl FnOnce() -> Result<T, Failure> + Send + 'static,
) -> Result<T, Failure> {
    let (tx, rx) = mpsc::channel();
    std::thread::Builder::new()
        .name("minkashot-grab".into())
        .spawn(move || {
            let _ = tx.send(f());
        })
        .map_err(|e| Failure::Capture(format!("cannot start the capture thread: {e}")))?;
    match rx.recv_timeout(timeout) {
        Ok(result) => result,
        Err(RecvTimeoutError::Timeout) => Err(Failure::Capture(format!(
            "the compositor did not answer within {} s",
            timeout.as_secs_f32()
        ))),
        Err(RecvTimeoutError::Disconnected) => {
            Err(Failure::Capture("the capture thread failed".into()))
        }
    }
}

/// The output a shot is of: the one named, or for none (`""` too) the
/// focused one, which the compositor tells (`currentMonitor` of
/// `workspaces.get`). When it does not, or names none of `outputs`, the
/// first output, with a note saying so.
pub fn choose<'a>(
    asked: Option<&str>,
    outputs: &'a [Output],
    focused: impl FnOnce() -> Result<String, String>,
) -> Result<&'a Output, Failure> {
    let names = || outputs.iter().map(|o| o.name.clone()).collect::<Vec<_>>();
    if outputs.is_empty() {
        return Err(Failure::Capture(
            "no outputs: is a Wayland compositor running here (WAYLAND_DISPLAY)?".into(),
        ));
    }
    if let Some(name) = asked.filter(|n| !n.is_empty()) {
        return outputs
            .iter()
            .find(|o| o.name == name)
            .ok_or_else(|| Failure::UnknownOutput {
                name: name.to_owned(),
                outputs: names(),
            });
    }
    let first = &outputs[0];
    match focused() {
        Ok(name) => match outputs.iter().find(|o| o.name == name) {
            Some(o) => Ok(o),
            None => {
                minka_note::note!(
                    "minkashot: the focused output {name:?} is not one of {}; taking {}",
                    names().join(", "),
                    first.name
                );
                Ok(first)
            }
        },
        Err(why) => {
            minka_note::note!(
                "minkashot: cannot tell the focused output ({why}); taking the first, {}",
                first.name
            );
            Ok(first)
        }
    }
}

/// The focused output's name, from the compositor's IPC.
pub fn focused_output() -> Result<String, String> {
    let view = minka_ipc::request_blocking("workspaces.get", serde_json::json!({}), FOCUS_TIMEOUT)
        .map_err(|e| format!("the compositor's IPC: {e}"))?;
    view.get("currentMonitor")
        .and_then(|m| m.as_str())
        .filter(|m| !m.is_empty())
        .map(str::to_owned)
        .ok_or_else(|| "workspaces.get names no currentMonitor".into())
}

/// One picture of an output or a window, at its own size, without the
/// pointer. `what` names it in a failure: "DP-1", "kitty: \"fish\"".
pub fn picture(
    target: impl Into<CaptureTarget>,
    what: &str,
    timeout: Duration,
) -> Result<Arc<Bitmap>, Failure> {
    let target = target.into();
    let window = matches!(target, CaptureTarget::Toplevel(_));
    let shot = capture(target, CaptureOptions::default());
    if !shot.available() {
        return Err(Failure::Capture(if window {
            format!(
                "cannot capture {what}: the compositor offers no window capture \
                 (ext-image-copy-capture of a foreign toplevel, with shm)"
            )
        } else {
            format!(
                "cannot capture {what}: the compositor offers no output capture \
                 (ext-image-copy-capture with shm)"
            )
        }));
    }
    let start = Instant::now();
    loop {
        if let Some(picture) = shot.picture() {
            return Ok(picture);
        }
        if shot.stopped() {
            return Err(Failure::Capture(if window {
                format!("the compositor would not capture {what} (has it closed?)")
            } else {
                format!("the compositor would not capture {what} (is it switched off?)")
            }));
        }
        if start.elapsed() >= timeout {
            return Err(Failure::Capture(format!(
                "no picture of {what} within {} s",
                timeout.as_secs_f32()
            )));
        }
        std::thread::sleep(Duration::from_millis(5));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn output(name: &str) -> Output {
        Output {
            name: name.into(),
            ..Default::default()
        }
    }

    #[test]
    fn a_named_output_is_that_one() {
        let outputs = [output("eDP-1"), output("DP-1")];
        let chosen = choose(Some("DP-1"), &outputs, || panic!("not asked")).unwrap();
        assert_eq!(chosen.name, "DP-1");
    }

    #[test]
    fn an_unknown_name_lists_the_outputs() {
        let outputs = [output("eDP-1"), output("DP-1")];
        match choose(Some("HDMI-A-9"), &outputs, || panic!("not asked")) {
            Err(Failure::UnknownOutput { name, outputs }) => {
                assert_eq!(name, "HDMI-A-9");
                assert_eq!(outputs, ["eDP-1", "DP-1"]);
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn no_name_is_the_focused_output() {
        let outputs = [output("eDP-1"), output("DP-1")];
        for asked in [None, Some("")] {
            let chosen = choose(asked, &outputs, || Ok("DP-1".into())).unwrap();
            assert_eq!(chosen.name, "DP-1");
        }
    }

    #[test]
    fn without_a_focused_output_it_is_the_first() {
        let outputs = [output("eDP-1"), output("DP-1")];
        let chosen = choose(None, &outputs, || Err("no answer".into())).unwrap();
        assert_eq!(chosen.name, "eDP-1");
        let chosen = choose(None, &outputs, || Ok("HDMI-A-3".into())).unwrap();
        assert_eq!(chosen.name, "eDP-1");
    }

    #[test]
    fn the_whole_wait_covers_its_parts() {
        assert!(SHOT_TIMEOUT > PICTURE_TIMEOUT + FOCUS_TIMEOUT);
        // A window's: the role, the list (the toolkit waits half a second
        // for it, and then this) and the picture.
        use crate::window::{LIST_TIMEOUT, ROLE_TIMEOUT};
        let list = Duration::from_millis(500) + LIST_TIMEOUT;
        assert!(SHOT_TIMEOUT > PICTURE_TIMEOUT + ROLE_TIMEOUT + list);
    }

    #[test]
    fn within_gives_what_comes_in_time() {
        assert_eq!(within(Duration::from_secs(5), || Ok(5)), Ok(5));
        assert_eq!(
            within::<()>(Duration::from_secs(5), || Err(Failure::Usage("x".into()))),
            Err(Failure::Usage("x".into()))
        );
    }

    #[test]
    fn within_gives_up_on_what_never_comes() {
        let start = Instant::now();
        let result = within::<()>(Duration::from_millis(100), || {
            loop {
                std::thread::park();
            }
        });
        assert!(start.elapsed() < Duration::from_secs(2));
        match result {
            Err(f @ Failure::Capture(_)) => assert_eq!(f.code(), 1),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn no_outputs_is_a_failed_capture() {
        assert!(matches!(
            choose(None, &[], || Ok("DP-1".into())),
            Err(Failure::Capture(_))
        ));
        assert!(matches!(
            choose(Some("DP-1"), &[], || Ok("DP-1".into())),
            Err(Failure::Capture(_))
        ));
    }
}
