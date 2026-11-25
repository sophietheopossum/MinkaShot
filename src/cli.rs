//! The command line: `minkashot full|region|win|list ...`, which maps no
//! surface and takes no input: it captures, crops, saves and says where.
//! Also `minkashot daemon`, and the commands to it (`daemon.rs`).

use anti_webui::core::images::Bitmap;
use anti_webui::shell::{LocalTime, Output, Toplevel};
use std::ffi::OsString;
use std::fmt;
use std::io::{self, IsTerminal, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::daemon::{self, Command};
use crate::geometry::{self, Crop, Rect};
use crate::window::{self, Seen};
use crate::{grab, save};

pub const USAGE: &str = "\
usage: minkashot full [<output>] [<path>]
       minkashot region <output> <x,y,w,h> [<path>]
       minkashot win [<window>] [<path>]
       minkashot list
       minkashot interactive | cancel | status
       minkashot daemon

Takes a screenshot of one output, of a region of it, or of one window, and
saves it as PNG. It shows nothing and takes no input. The path written is
printed. A window is pictured as it is, even under others, and is not raised
or focused. list prints each window's app id and title, a tab between.

  <output>   a name like eDP-1 or DP-1; left out or \"\", the focused output
  <x,y,w,h>  the region, in the output's logical pixels from its top left
  <window>   a role (minkamon.disk), a whole title, an app id or a piece of a
             title or app id, tried in that order; left out or \"\", the
             focused window. Of several that match, the focused one, else none
  <path>     where to save; left out or \"\",
             ~/Pictures/Screenshots/minkashot-yyyyMMdd-hhmmss.png;
             - writes the PNG to standard output instead and prints no path
             (minkashot full \"\" - | wl-copy -t image/png), and ./- is a file

daemon runs the interactive screenshot's daemon, one per session, which
Print arms: it freezes every output, and a drag, or a window picked with the
toolbar's Window, makes a region. Esc lets go; Enter saves the region of the
output the pointer is on, or all of it, under the default name, and the
toolbar's copy puts it on the clipboard as image/png. Its retry freezes again
after the delay it sets, and Print during that countdown freezes at once.
Nothing else lets go but cancel, which also stops a countdown. interactive
arms it as Print does, cancel puts it back to idle, and status says what it
is doing; each prints the daemon's answer. The session's Print is still the
QML daemon's.

Exit status: 0 saved or answered, 1 the capture failed or timed out,
2 usage, 3 no output or window of that name (or several windows, none
focused), 4 the file could not be written (a full disk, or a path that is a
device, a pipe or a directory), 5 no daemon is running, 6 the daemon did
not answer, or its lock could not be read.";

/// What went wrong, and the exit status that says so.
#[derive(Debug, PartialEq)]
pub enum Failure {
    /// No picture: no compositor, no capture, the output off, or too slow.
    Capture(String),
    /// The arguments are wrong, or the region misses the output.
    Usage(String),
    /// No output has this name.
    UnknownOutput { name: String, outputs: Vec<String> },
    /// No window is the one asked for: none matches, or several do.
    NoWindow(String),
    /// The picture could not be saved.
    Write(String),
    /// A command for the daemon, and none is running.
    NoDaemon,
    /// A command for the daemon, which did not answer, or whose lock could
    /// not be read.
    NoAnswer(String),
}

impl Failure {
    pub fn code(&self) -> u8 {
        match self {
            Failure::Capture(_) => 1,
            Failure::Usage(_) => 2,
            Failure::UnknownOutput { .. } | Failure::NoWindow(_) => 3,
            Failure::Write(_) => 4,
            Failure::NoDaemon => 5,
            Failure::NoAnswer(_) => 6,
        }
    }
}

impl fmt::Display for Failure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Failure::Capture(why)
            | Failure::Usage(why)
            | Failure::Write(why)
            | Failure::NoWindow(why)
            | Failure::NoAnswer(why) => f.write_str(why),
            Failure::NoDaemon => f.write_str("no minkashot daemon is running"),
            Failure::UnknownOutput { name, outputs } => {
                write!(
                    f,
                    "no output named {name:?}; the outputs are: {}",
                    outputs.join(", ")
                )?;
                if name.contains('/') || name.ends_with(".png") {
                    write!(f, " (to give only a path, pass \"\" as the output first)")?;
                }
                Ok(())
            }
        }
    }
}

/// What a shot is of.
#[derive(Debug, PartialEq)]
pub enum Subject {
    Output {
        /// `None`: the focused output.
        name: Option<String>,
        /// `None`: the whole output.
        region: Option<Rect>,
    },
    /// What picks the window (see [`window::pick`]); `None`: the focused one.
    Window(Option<String>),
}

/// A shot the command line asks for.
#[derive(Debug, PartialEq)]
pub struct Shot {
    pub of: Subject,
    /// `None`: the default path.
    pub path: Option<PathBuf>,
}

/// What the arguments ask for.
#[derive(Debug, PartialEq)]
pub enum Request {
    Shot(Shot),
    /// The windows, one per line.
    List,
    /// Run the daemon.
    Daemon,
    /// Send this to the daemon.
    Tell(Command),
    Help,
}

/// Reads the arguments after the program's name.
pub fn parse(args: &[OsString]) -> Result<Request, Failure> {
    let Some((verb, rest)) = args.split_first() else {
        return Err(Failure::Usage("no command".into()));
    };
    let text = |arg: &OsString, what: &str| {
        arg.to_str()
            .map(str::to_owned)
            .ok_or_else(|| Failure::Usage(format!("the {what} is not UTF-8")))
    };
    let named = |arg: Option<&OsString>, what: &str| -> Result<Option<String>, Failure> {
        Ok(match arg {
            Some(a) => Some(text(a, what)?).filter(|n| !n.is_empty()),
            None => None,
        })
    };
    let path = |arg: Option<&OsString>| arg.filter(|p| !p.is_empty()).map(PathBuf::from);
    let too_many = |verb: &str| Failure::Usage(format!("too many arguments for {verb}"));
    if let Some(command) = verb.to_str().and_then(Command::from_word) {
        if !rest.is_empty() {
            return Err(too_many(command.word()));
        }
        return Ok(Request::Tell(command));
    }
    match verb.to_str() {
        Some("full") => {
            if rest.len() > 2 {
                return Err(too_many("full"));
            }
            Ok(Request::Shot(Shot {
                of: Subject::Output {
                    name: named(rest.first(), "output name")?,
                    region: None,
                },
                path: path(rest.get(1)),
            }))
        }
        Some("region") => {
            if rest.len() < 2 {
                return Err(Failure::Usage(
                    "region needs an output (or \"\") and x,y,w,h".into(),
                ));
            }
            if rest.len() > 3 {
                return Err(too_many("region"));
            }
            let spec = text(&rest[1], "region")?;
            let region = geometry::parse_spec(&spec).map_err(Failure::Usage)?;
            Ok(Request::Shot(Shot {
                of: Subject::Output {
                    name: named(rest.first(), "output name")?,
                    region: Some(region),
                },
                path: path(rest.get(2)),
            }))
        }
        Some("win") => {
            if rest.len() > 2 {
                return Err(too_many("win"));
            }
            // A piece of a title matches, and many have a "-" in them.
            if rest.first().is_some_and(|w| w == "-") {
                return Err(Failure::Usage(
                    "the window comes before the path: win \"\" - for the focused one".into(),
                ));
            }
            Ok(Request::Shot(Shot {
                of: Subject::Window(named(rest.first(), "window")?),
                path: path(rest.get(1)),
            }))
        }
        Some("list") => {
            if !rest.is_empty() {
                return Err(too_many("list"));
            }
            Ok(Request::List)
        }
        Some("daemon") => {
            if !rest.is_empty() {
                return Err(too_many("daemon"));
            }
            Ok(Request::Daemon)
        }
        Some("help" | "--help" | "-h") => Ok(Request::Help),
        _ => Err(Failure::Usage(format!(
            "unknown command {:?}",
            verb.to_string_lossy()
        ))),
    }
}

/// Where a shot asked for goes: `-` is standard output, and a relative
/// path is made absolute, so the path printed is one to use from anywhere.
pub(crate) fn target(path: Option<&Path>) -> Result<save::Target, Failure> {
    Ok(match path {
        Some(p) if p.as_os_str() == "-" => save::Target::Stdout,
        Some(p) => save::Target::Path(std::path::absolute(p).unwrap_or_else(|_| p.to_owned())),
        None => save::Target::Default {
            dir: save::default_dir()
                .ok_or_else(|| Failure::Write("no $HOME for ~/Pictures/Screenshots".into()))?,
            name: save::default_name(&LocalTime::now()),
        },
    })
}

/// Takes the shot and saves it, and gives the path to print: none when the
/// PNG went to standard output.
pub fn shoot(shot: &Shot) -> Result<Option<PathBuf>, Failure> {
    // Where it goes is settled first, so a missing $HOME costs no capture.
    let target = target(shot.path.as_deref())?;
    if target == save::Target::Stdout && std::io::stdout().is_terminal() {
        return Err(Failure::Usage(
            "standard output is a terminal: pipe the PNG somewhere (| wl-copy -t image/png)".into(),
        ));
    }
    let (crop, picture) = match &shot.of {
        Subject::Output { name, region } => output_picture(name.clone(), *region)?,
        Subject::Window(asked) => window_picture(asked.clone())?,
    };
    write(&picture, crop, &target)
}

/// Saves `crop` of `picture`, opaque, as a PNG at `target`, and gives the
/// path written: none for standard output.
pub(crate) fn write(
    picture: &Bitmap,
    crop: Crop,
    target: &save::Target,
) -> Result<Option<PathBuf>, Failure> {
    let bytes = png(picture, crop)?;
    save::save(target, &bytes).map_err(|e| cannot_write(target, e))
}

/// The bytes of a PNG file of `crop` of `picture`, opaque.
pub(crate) fn png(picture: &Bitmap, crop: Crop) -> Result<Vec<u8>, Failure> {
    let rgba = save::crop_rgba(picture, crop)
        .ok_or_else(|| Failure::Capture("the picture's pixels are gone".into()))?;
    anti_webui::app::headless::encode_png(crop.width, crop.height, &rgba)
        .map_err(|e| Failure::Write(format!("cannot encode the PNG: {e}")))
}

/// An output's picture, and the crop of it a shot asks for.
fn output_picture(
    name: Option<String>,
    region: Option<Rect>,
) -> Result<(Crop, Arc<Bitmap>), Failure> {
    // Everything that waits on the compositor, under one bound.
    let (output, picture) = grab::within(grab::SHOT_TIMEOUT, move || {
        grab_for(name.as_deref(), region.as_ref())
    })?;
    let logical = (output.logical_size.width, output.logical_size.height);
    let size = (picture.width(), picture.height());
    let crop = geometry::crop(region.as_ref(), logical, output.scale, size).ok_or_else(|| {
        Failure::Usage(format!(
            "the region is off {}'s {}x{} picture",
            output.name, size.0, size.1
        ))
    })?;
    Ok((crop, picture))
}

/// A picture of the window asked for, all of it.
fn window_picture(asked: Option<String>) -> Result<(Crop, Arc<Bitmap>), Failure> {
    let picture = grab::within(grab::SHOT_TIMEOUT, move || {
        let (window, what) = window_for(asked.as_deref())?;
        grab::picture(window, &what, grab::PICTURE_TIMEOUT)
    })?;
    Ok((Crop::whole(picture.width(), picture.height()), picture))
}

/// Every window, as the toolkit lists them, or why they cannot be had. A
/// list that a slow compositor has not finished sending would leave a
/// window out, and that window would then be "no window of that name".
fn toplevels() -> Result<Vec<Toplevel>, Failure> {
    let list = anti_webui::shell::toplevels();
    if !list.available() {
        return Err(Failure::Capture(
            "the compositor lists no windows (no foreign-toplevel protocol), \
             or there is no Wayland compositor here (WAYLAND_DISPLAY)"
                .into(),
        ));
    }
    let start = Instant::now();
    while !list.complete() {
        if start.elapsed() >= window::LIST_TIMEOUT {
            return Err(Failure::Capture(format!(
                "the compositor did not finish listing its windows within {} s",
                window::LIST_TIMEOUT.as_secs_f32() + 0.5
            )));
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    Ok(list.all())
}

/// The window asked for, and how to name it.
fn window_for(asked: Option<&str>) -> Result<(Toplevel, String), Failure> {
    let windows = toplevels()?;
    let seen: Vec<Seen> = windows.iter().map(Seen::from).collect();
    let at = window::pick(asked, &seen, window::roles)?;
    Ok((windows[at].clone(), window::describe(&seen[at])))
}

/// `minkashot list`: each window's app id and title, a tab between.
fn list() -> Result<(), Failure> {
    let windows = grab::within(grab::SHOT_TIMEOUT, toplevels)?;
    let text: String = windows
        .iter()
        .map(|w| format!("{}\t{}\n", w.app_id, w.title))
        .collect();
    std::io::stdout()
        .lock()
        .write_all(text.as_bytes())
        .map_err(|e| Failure::Write(format!("cannot write the list: {e}")))
}

/// Any failure to save, a full disk or a path that is no file, is exit
/// status 4.
fn cannot_write(target: &save::Target, e: io::Error) -> Failure {
    let where_to = match target {
        save::Target::Path(p) => p.display().to_string(),
        save::Target::Default { dir, name } => dir.join(name).display().to_string(),
        save::Target::Stdout => "standard output".into(),
    };
    Failure::Write(format!("cannot write {where_to}: {e}"))
}

/// The output a shot is of, and one picture of it. It talks to the
/// compositor, so [`shoot`] runs it under [`grab::within`].
fn grab_for(asked: Option<&str>, region: Option<&Rect>) -> Result<(Output, Arc<Bitmap>), Failure> {
    let outputs = anti_webui::shell::outputs();
    let output = grab::choose(asked, &outputs, grab::focused_output)?.clone();
    let logical = (output.logical_size.width, output.logical_size.height);
    if let Some(r) = region
        && logical.0 > 0
        && logical.1 > 0
        && !geometry::overlaps(r, logical)
    {
        return Err(Failure::Usage(format!(
            "the region {},{},{},{} is off {}, which is {}x{} logical pixels",
            r.x, r.y, r.width, r.height, output.name, logical.0, logical.1
        )));
    }
    let picture = grab::picture(&output, &output.name, grab::PICTURE_TIMEOUT)?;
    Ok((output, picture))
}

/// Runs the command line and returns the exit status.
pub fn run(args: &[OsString]) -> u8 {
    let request = match parse(args) {
        Ok(r) => r,
        Err(f) => {
            minka_note::note!("minkashot: {f}\n{USAGE}");
            return f.code();
        }
    };
    let shot = match request {
        Request::Help => {
            let _ = writeln!(std::io::stdout().lock(), "{USAGE}");
            return 0;
        }
        Request::List => {
            return match list() {
                Ok(()) => 0,
                Err(f) => {
                    minka_note::note!("minkashot: {f}");
                    f.code()
                }
            };
        }
        Request::Daemon => return daemon::run(),
        Request::Tell(command) => {
            return match daemon::tell(command) {
                Ok(answer) => {
                    let _ = writeln!(std::io::stdout().lock(), "{answer}");
                    0
                }
                Err(f) => {
                    minka_note::note!("minkashot: {f}");
                    f.code()
                }
            };
        }
        Request::Shot(shot) => shot,
    };
    match shoot(&shot) {
        Ok(path) => {
            if let Some(path) = path {
                // The shot is saved even if nobody reads this.
                let mut out = std::io::stdout().lock();
                let _ = out.write_all(path.as_os_str().as_encoded_bytes());
                let _ = out.write_all(b"\n");
            }
            0
        }
        Err(f) => {
            minka_note::note!("minkashot: {f}");
            f.code()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(list: &[&str]) -> Vec<OsString> {
        list.iter().map(OsString::from).collect()
    }

    fn shot(output: Option<&str>, region: Option<Rect>, path: Option<&str>) -> Request {
        Request::Shot(Shot {
            of: Subject::Output {
                name: output.map(str::to_owned),
                region,
            },
            path: path.map(PathBuf::from),
        })
    }

    fn win(window: Option<&str>, path: Option<&str>) -> Request {
        Request::Shot(Shot {
            of: Subject::Window(window.map(str::to_owned)),
            path: path.map(PathBuf::from),
        })
    }

    #[test]
    fn win_takes_a_window_and_a_path() {
        assert_eq!(parse(&args(&["win"])), Ok(win(None, None)));
        assert_eq!(
            parse(&args(&["win", "minkamon.disk"])),
            Ok(win(Some("minkamon.disk"), None))
        );
        assert_eq!(parse(&args(&["win", "", "-"])), Ok(win(None, Some("-"))));
        assert_eq!(parse(&args(&["list"])), Ok(Request::List));
    }

    #[test]
    fn the_daemon_and_its_commands_take_nothing_more() {
        assert_eq!(parse(&args(&["daemon"])), Ok(Request::Daemon));
        assert_eq!(
            parse(&args(&["interactive"])),
            Ok(Request::Tell(Command::Interactive))
        );
        assert_eq!(
            parse(&args(&["cancel"])),
            Ok(Request::Tell(Command::Cancel))
        );
        assert_eq!(
            parse(&args(&["status"])),
            Ok(Request::Tell(Command::Status))
        );
        for bad in [
            &["daemon", "now"][..],
            &["status", "-v"],
            &["cancel", ""],
            &["interactive", "DP-1"],
            &["Status"],
        ] {
            match parse(&args(bad)) {
                Err(f @ Failure::Usage(_)) => assert_eq!(f.code(), 2),
                other => panic!("{bad:?}: {other:?}"),
            }
        }
    }

    #[test]
    fn full_takes_an_output_and_a_path() {
        assert_eq!(parse(&args(&["full"])), Ok(shot(None, None, None)));
        assert_eq!(
            parse(&args(&["full", "DP-1"])),
            Ok(shot(Some("DP-1"), None, None))
        );
        assert_eq!(
            parse(&args(&["full", "", "/x.png"])),
            Ok(shot(None, None, Some("/x.png")))
        );
        assert_eq!(
            parse(&args(&["full", "eDP-1", ""])),
            Ok(shot(Some("eDP-1"), None, None))
        );
    }

    #[test]
    fn region_takes_an_output_a_spec_and_a_path() {
        let r = Rect {
            x: 1.0,
            y: 2.0,
            width: 3.0,
            height: 4.0,
        };
        assert_eq!(
            parse(&args(&["region", "", "1,2,3,4"])),
            Ok(shot(None, Some(r), None))
        );
        assert_eq!(
            parse(&args(&["region", "DP-1", "1,2,3,4", "a.png"])),
            Ok(shot(Some("DP-1"), Some(r), Some("a.png")))
        );
    }

    #[test]
    fn wrong_arguments_are_usage() {
        let cases: [&[&str]; 11] = [
            &[],
            &["shoot"],
            &["win", "-"],
            &["win", "kitty", "a.png", "more"],
            &["list", "more"],
            &["full", "DP-1", "a.png", "more"],
            &["region"],
            &["region", "DP-1"],
            &["region", "DP-1", "1,2,3"],
            &["region", "DP-1", "1,2,0,4"],
            &["region", "DP-1", "1,2,3,4", "a.png", "more"],
        ];
        for bad in cases {
            match parse(&args(bad)) {
                Err(f @ Failure::Usage(_)) => assert_eq!(f.code(), 2),
                other => panic!("{bad:?}: {other:?}"),
            }
        }
        assert_eq!(parse(&args(&["--help"])), Ok(Request::Help));
    }

    #[test]
    fn a_dash_is_standard_output() {
        assert_eq!(
            parse(&args(&["full", "", "-"])),
            Ok(shot(None, None, Some("-")))
        );
        assert_eq!(target(Some(Path::new("-"))), Ok(save::Target::Stdout));
        match target(Some(Path::new("./-"))) {
            Ok(save::Target::Path(p)) => assert!(p.is_absolute() && p.ends_with("-"), "{p:?}"),
            other => panic!("{other:?}"),
        }
        match target(Some(Path::new("shot.png"))) {
            Ok(save::Target::Path(p)) => assert!(p.is_absolute(), "{p:?}"),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn a_write_failure_is_exit_status_4() {
        let target = save::Target::Path("/x.png".into());
        let full = cannot_write(&target, io::ErrorKind::StorageFull.into());
        assert_eq!(full.code(), 4);
        assert!(
            full.to_string().starts_with("cannot write /x.png: "),
            "{full}"
        );
        // A path that is no file says how to get the PNG down a pipe.
        let null = save::Target::Path("/dev/null".into());
        let refused = cannot_write(&null, save::save(&null, b"png").unwrap_err());
        assert_eq!(refused.code(), 4);
        assert!(
            refused.to_string().contains("give - as the path"),
            "{refused}"
        );
    }

    #[test]
    fn each_failure_has_its_exit_status() {
        assert_eq!(Failure::Capture("x".into()).code(), 1);
        assert_eq!(Failure::Usage("x".into()).code(), 2);
        let unknown = Failure::UnknownOutput {
            name: "X".into(),
            outputs: vec!["eDP-1".into()],
        };
        assert_eq!(unknown.code(), 3);
        assert_eq!(Failure::Write("x".into()).code(), 4);
        assert_eq!(Failure::NoWindow("x".into()).code(), 3);
        assert_eq!(Failure::NoDaemon.code(), 5);
        assert_eq!(Failure::NoAnswer("x".into()).code(), 6);
    }

    #[test]
    fn an_unknown_output_lists_the_outputs() {
        let f = Failure::UnknownOutput {
            name: "HDMI-A-9".into(),
            outputs: vec!["eDP-1".into(), "DP-1".into()],
        };
        assert_eq!(
            f.to_string(),
            "no output named \"HDMI-A-9\"; the outputs are: eDP-1, DP-1"
        );
        let f = Failure::UnknownOutput {
            name: "/tmp/a.png".into(),
            outputs: vec!["eDP-1".into()],
        };
        assert!(f.to_string().contains("pass \"\" as the output"), "{f}");
    }
}
