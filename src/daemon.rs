//! `minkashot daemon`: the resident process that Print arms, and the
//! commands sent to it (`minkashot interactive|cancel|status`).
//!
//! One runs per session: it holds `$XDG_RUNTIME_DIR/minkashot.lock`
//! (minka-instance), and the commands are request lines on that socket. It
//! hears Print as the compositor's `ui.minkashot` broadcast. Both come in on
//! threads of their own, which block and hand what matters to the UI thread
//! through one inbox; nothing polls. The UI thread steps the state machine
//! (`state.rs`) and carries out its effects.
//!
//! An arm captures every output at once, each on a thread of its own, and
//! only once the frames are in hand opens a layer over each output that gave
//! one, showing its frame (`overlay.rs`), where a drag or a picked window
//! makes a region. The frames and the regions stay here, and the machine
//! knows only the outputs' names. The keyboard is on the overlay the pointer
//! is on, and goes with it. Esc closes them. Enter saves the frame of the
//! output the pointer is on, its region or all of it, under the default
//! name, and the toolbar's copy puts it on the clipboard; either is written
//! with the frames still up, as in the QML, which close once it is done, or
//! stay, saying why, if it failed. The toolbar's retry closes them and
//! freezes again, after a countdown on a layer of its own if there is a
//! delay. Nothing else closes them, as in the QML. Each arm logs how long
//! each step took after the Print or the retry that began it.

use anti_webui::core::animation::{request_frame, set_timeout};
use anti_webui::core::channel::{UiSender, ui_channel};
use anti_webui::core::images::Bitmap;
use anti_webui::core::reactive::{Signal, on_cleanup, signal};
use anti_webui::core::window::current_window;
use anti_webui::shell::{
    Anchor, Exclusive, Keyboard, Layer, LayerHandle, LayerOptions, LocalTime, Margin, Output,
    Shell, open_layer,
};
use minka_instance::{Claim, Instance, RequestError};
use minka_ipc::{IpcClient, IpcEvent};
use serde_json::{Value, json};
use std::cell::{Cell, OnceCell, RefCell};
use std::collections::VecDeque;
use std::fmt;
use std::rc::Rc;
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use crate::cli::{self, Failure};
use crate::geometry::{self, Crop, Rect};
use crate::overlay::{self, Frozen, Shared, Told};
use crate::state::{Effect, Event, Layout, Machine, To};
use crate::{countdown, grab, theme, window};

/// The lock's name: `$XDG_RUNTIME_DIR/minkashot.lock`.
const NAME: &str = "minkashot";

/// How long a command waits for the daemon's answer.
const ASK_TIMEOUT: Duration = Duration::from_secs(2);

/// What can be asked of a running daemon. The request line is the word.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Command {
    /// Arm, as Print does.
    Interactive,
    /// Back to idle, closing whatever is open.
    Cancel,
    /// What it is doing.
    Status,
}

impl Command {
    pub fn word(self) -> &'static str {
        match self {
            Command::Interactive => "interactive",
            Command::Cancel => "cancel",
            Command::Status => "status",
        }
    }

    pub fn from_word(word: &str) -> Option<Self> {
        [Command::Interactive, Command::Cancel, Command::Status]
            .into_iter()
            .find(|c| c.word() == word)
    }
}

/// Sends `command` to the running daemon and gives its one-line answer.
pub fn tell(command: Command) -> Result<String, Failure> {
    minka_instance::request(NAME, command.word(), ASK_TIMEOUT).map_err(|e| match e {
        RequestError::NotRunning => Failure::NoDaemon,
        // Nothing says a daemon runs: the lock could not be reached or read.
        RequestError::Io(e) => Failure::NoAnswer(format!("cannot ask the minkashot daemon: {e}")),
        e => Failure::NoAnswer(format!("the minkashot daemon is {e}")),
    })
}

/// A moment as the log and the status show it: local time to the
/// millisecond.
fn stamp(at: SystemTime) -> String {
    let since = at.duration_since(UNIX_EPOCH).unwrap_or_default();
    let time = LocalTime::at(since.as_secs() as i64).format("%H:%M:%S");
    format!("{time}.{:03}", since.subsec_millis())
}

/// A line for the daemon's log, with the time.
fn log(text: fmt::Arguments) {
    minka_note::note!("minkashot: {} {text}", stamp(SystemTime::now()));
}

fn ms(d: Duration) -> f64 {
    d.as_secs_f64() * 1000.0
}

/// The daemon's resident memory (`VmRSS`), for the status: what the frames
/// take while they are up, and that it comes back after.
fn resident() -> String {
    std::fs::read_to_string("/proc/self/status")
        .ok()
        .and_then(|s| {
            s.lines()
                .find_map(|l| l.strip_prefix("VmRSS:").map(|v| v.trim().to_owned()))
        })
        .unwrap_or_else(|| "unknown".into())
}

/// Runs the daemon until the process ends, and gives the exit status. A
/// second one exits at once with 0: minka-instance has logged which PID
/// holds the lock.
pub fn run() -> u8 {
    let Claim::Primary(instance) = minka_instance::claim(NAME) else {
        return 0;
    };
    let today = LocalTime::now();
    log(format_args!(
        "daemon started {}/{}/{}, pid {}",
        today.day,
        today.month,
        today.year,
        std::process::id()
    ));
    match Shell::new()
        .namespace("minkashot")
        .run(move || root(instance))
    {
        Ok(()) => 0,
        Err(e) => {
            log(format_args!("cannot run: {e}"));
            1
        }
    }
}

/// What the threads hand the UI thread.
enum Msg {
    /// The Print broadcast, and when it came.
    Print(SystemTime),
    /// The compositor's IPC (re)connected, or was lost.
    Connected(bool),
    /// The answer to a request to the compositor.
    Answer {
        id: u64,
        result: Result<Value, Value>,
    },
    /// A command from `minkashot interactive|cancel|status`.
    Request(minka_instance::Request),
    /// A frame of an output from its capture thread, or why there is none,
    /// and how long it took.
    Frame {
        generation: u64,
        output: Output,
        frame: Result<Arc<Bitmap>, String>,
        took: Duration,
    },
    /// What an overlay told.
    Overlay(Told),
}

/// The outputs being captured.
struct Capturing {
    /// The generation of the capture's phase.
    generation: u64,
    /// Every output, in the compositor's order.
    order: Vec<String>,
    /// How many have not answered yet.
    left: usize,
    frames: Vec<(Output, Arc<Bitmap>)>,
}

/// An output's overlay, while it is up.
struct Overlay {
    output: String,
    /// Its logical size and scale, which the region is cropped by.
    logical: (i32, i32),
    scale: f32,
    frame: Arc<Bitmap>,
    /// The region the overlay has, in its logical pixels, if any.
    region: Rc<Cell<Option<Rect>>>,
    layer: LayerHandle,
}

/// What the daemon keeps on the UI thread.
struct Daemon {
    machine: RefCell<Machine>,
    ipc: Option<Arc<IpcClient>>,
    connected: Cell<bool>,
    /// What the overlays show the same: the machine's pick mode and delay,
    /// as it last said, and the failure to say.
    shared: Shared,
    /// The countdown's layer, while it is open, and the seconds it shows.
    countdown: RefCell<Option<LayerHandle>>,
    left: Signal<u8>,
    /// The `workspaces.get` in flight: its id, the generation it answers,
    /// and when it was asked.
    fetch: Cell<Option<(u64, u64, Instant)>>,
    /// The root's inbox, which the threads and the overlays send through.
    inbox: OnceCell<UiSender<Msg>>,
    /// When the arm began, and what began it (Print or Retry), which the
    /// timings in the log count from.
    armed_at: Cell<Option<(Instant, &'static str)>>,
    capturing: RefCell<Option<Capturing>>,
    /// The frames of the capture just finished, for the overlays it opens.
    frames: RefCell<Vec<(Output, Arc<Bitmap>)>>,
    overlays: RefCell<Vec<Overlay>>,
}

/// Runs on the UI thread, inside the shell's root: the inbox and its
/// threads.
fn root(instance: Instance) {
    // How what the layers show is drawn, as in every Minka app.
    theme::install();
    // The shell's own connection, and the outputs, now rather than at Print.
    anti_webui::shell::outputs();

    let ipc = IpcClient::spawn();
    let daemon = Rc::new(Daemon {
        machine: RefCell::new(Machine::new(Instant::now())),
        ipc: ipc.as_ref().ok().map(|(client, _)| client.clone()),
        connected: Cell::new(false),
        shared: Shared {
            pick: signal(false),
            delay: signal(0),
            failure: signal(None),
        },
        countdown: RefCell::new(None),
        left: signal(0),
        fetch: Cell::new(None),
        inbox: OnceCell::new(),
        armed_at: Cell::new(None),
        capturing: RefCell::new(None),
        frames: RefCell::new(Vec::new()),
        overlays: RefCell::new(Vec::new()),
    });
    let handler = daemon.clone();
    let inbox = ui_channel(move |msg: Msg| handler.handle(msg));
    let _ = daemon.inbox.set(inbox.clone());

    match ipc {
        Ok((_, events)) => {
            let inbox = inbox.clone();
            let spawned = std::thread::Builder::new()
                .name("minkashot-ipc".into())
                .spawn(move || {
                    while let Ok(event) = events.recv() {
                        let msg = match event {
                            IpcEvent::Broadcast { event, payload } if event == "ui.minkashot" => {
                                match payload.get("action").and_then(Value::as_str) {
                                    Some("interactive") => Msg::Print(SystemTime::now()),
                                    _ => {
                                        log(format_args!("ignored ui.minkashot {payload}"));
                                        continue;
                                    }
                                }
                            }
                            IpcEvent::Broadcast { .. } => continue,
                            IpcEvent::Connected => Msg::Connected(true),
                            IpcEvent::Disconnected => Msg::Connected(false),
                            IpcEvent::Response { id, result } => Msg::Answer { id, result },
                        };
                        inbox.send(msg);
                    }
                });
            if let Err(e) = spawned {
                log(format_args!("cannot listen to the compositor's IPC: {e}"));
            }
        }
        Err(e) => log(format_args!("no compositor IPC, so no Print: {e}")),
    }

    let spawned = std::thread::Builder::new()
        .name("minkashot-requests".into())
        .spawn(move || {
            // A launch that gave way needs nothing: it has logged the PID.
            for event in instance.events() {
                if let minka_instance::Event::Request(request) = event {
                    inbox.send(Msg::Request(request));
                }
            }
        });
    if let Err(e) = spawned {
        log(format_args!("cannot take commands: {e}"));
    }
}

impl Daemon {
    fn handle(self: &Rc<Self>, msg: Msg) {
        match msg {
            // Each trigger is logged with what the daemon was doing, which
            // says why it does nothing when it does nothing.
            Msg::Print(at) => {
                let late = SystemTime::now().duration_since(at).unwrap_or_default();
                log(format_args!(
                    "Print: broadcast received at {}, handled {:.1} ms later, {}",
                    stamp(at),
                    late.as_secs_f64() * 1000.0,
                    self.machine.borrow().describe()
                ));
                let heard = Instant::now().checked_sub(late);
                self.interactive(heard.unwrap_or_else(Instant::now));
            }
            Msg::Connected(connected) => {
                self.connected.set(connected);
                let state = if connected { "connected" } else { "lost" };
                log(format_args!("the compositor's IPC is {state}"));
            }
            Msg::Answer { id, result } => self.answered(id, result),
            Msg::Request(request) => {
                let command = Command::from_word(request.line().trim());
                if let Some(c @ (Command::Interactive | Command::Cancel)) = command {
                    let now = self.machine.borrow().describe();
                    log(format_args!("minkashot {}, {now}", c.word()));
                }
                let answer = match command {
                    Some(Command::Interactive) => self.interactive(Instant::now()),
                    Some(Command::Cancel) => self.cancel(),
                    Some(Command::Status) => self.status(),
                    None => format!("unknown request {:?}", request.line()),
                };
                request.answer(&answer);
            }
            Msg::Frame {
                generation,
                output,
                frame,
                took,
            } => self.framed(generation, output, frame, took),
            Msg::Overlay(told) => self.told(told),
        }
    }

    fn inbox(&self) -> &UiSender<Msg> {
        self.inbox
            .get()
            .expect("the root makes the inbox before anything is sent")
    }

    /// How long after the arm `at` was, for the log.
    fn after_arm(&self, at: Instant) -> String {
        match self.armed_at.get() {
            Some((armed, by)) => format!(
                "{:.1} ms after {by}",
                ms(at.saturating_duration_since(armed))
            ),
            None => "with no Print".into(),
        }
    }

    /// What the machine is doing, and its generation. Every change of phase
    /// is a new generation, so an unchanged one says an event was not taken.
    fn seen(&self) -> (String, u64) {
        let machine = self.machine.borrow();
        (machine.describe(), machine.generation())
    }

    /// Print, or `minkashot interactive`, heard `at`; the answer to the
    /// latter. Whether it arms is the machine's rule.
    fn interactive(self: &Rc<Self>, at: Instant) -> String {
        let (before, generation) = self.seen();
        let armed = self.armed_at.replace(Some((at, "Print")));
        self.feed(Event::Interactive);
        let (now, after) = self.seen();
        if after == generation {
            self.armed_at.set(armed);
            return format!("already {before}; nothing done");
        }
        format!("started; now {now}")
    }

    /// `minkashot cancel`, and its answer.
    fn cancel(self: &Rc<Self>) -> String {
        let (before, generation) = self.seen();
        self.feed(Event::Cancel);
        if self.seen().1 == generation {
            return "idle; nothing to cancel".into();
        }
        format!("cancelled; was {before}")
    }

    fn status(&self) -> String {
        let machine = self.machine.borrow();
        let since = SystemTime::now()
            .checked_sub(machine.since().elapsed())
            .unwrap_or(UNIX_EPOCH);
        let ipc = if self.connected.get() {
            "connected"
        } else {
            "not connected"
        };
        format!(
            "{} since {}; delay {} s; compositor IPC {ipc}; pid {}, resident {}",
            machine.describe(),
            stamp(since),
            machine.delay(),
            std::process::id(),
            resident()
        )
    }

    /// The compositor's answer to the window list asked for, if it is that.
    fn answered(self: &Rc<Self>, id: u64, result: Result<Value, Value>) {
        let Some((asked, generation, at)) = self.fetch.get().filter(|f| f.0 == id) else {
            return;
        };
        self.fetch.set(None);
        // The machine ignores an answer for an arm that is over: say so.
        let late = if generation == self.machine.borrow().generation() {
            ""
        } else {
            " (too late: ignored)"
        };
        let layout = match result {
            Ok(view) => {
                let layout = Layout {
                    current: window::current_monitor(&view),
                    rects: window::rects(&view),
                };
                log(format_args!(
                    "window list {asked} in {:.1} ms: {} windows, the pointer on {}{late}",
                    at.elapsed().as_secs_f64() * 1000.0,
                    layout.rects.len(),
                    layout.current.as_deref().unwrap_or("no output")
                ));
                Some(layout)
            }
            Err(e) => {
                log(format_args!("window list {asked} failed: {e}{late}"));
                None
            }
        };
        self.feed(Event::Windows { generation, layout });
    }

    /// Steps the machine with `event`, then with what carrying out its
    /// effects gives straight back, until nothing is left.
    fn feed(self: &Rc<Self>, event: Event) {
        let mut queue = VecDeque::from([event]);
        while let Some(event) = queue.pop_front() {
            let effects = self.machine.borrow_mut().step(event, Instant::now());
            for effect in effects {
                queue.extend(self.carry_out(effect));
            }
        }
        // What the overlays show of it, the machine let go of first: what
        // runs on a change must be free to look at it.
        let (pick, delay) = {
            let machine = self.machine.borrow();
            (machine.pick(), machine.delay())
        };
        self.shared.pick.set_if_changed(pick);
        self.shared.delay.set_if_changed(delay);
    }

    /// Carries out one effect: an event when its answer is at hand.
    fn carry_out(self: &Rc<Self>, effect: Effect) -> Option<Event> {
        match effect {
            Effect::FetchWindows { generation } => {
                let id = self
                    .ipc
                    .as_ref()
                    .and_then(|c| c.request("workspaces.get", json!({})));
                let Some(id) = id else {
                    log(format_args!(
                        "the compositor's IPC is not connected: no window list"
                    ));
                    return Some(Event::Windows {
                        generation,
                        layout: None,
                    });
                };
                self.fetch.set(Some((id, generation, Instant::now())));
                log(format_args!("asked for the window list ({id})"));
                None
            }
            Effect::Capture { generation } => self.capture(generation),
            Effect::OpenOverlays { outputs, keyboard } => self.open_overlays(&outputs, &keyboard),
            Effect::CloseOverlays => {
                self.close_overlays();
                None
            }
            Effect::Keyboard { output } => {
                self.hand_keyboard(&output);
                None
            }
            Effect::Save {
                output,
                to,
                generation,
            } => self.save(&output, to, generation),
            Effect::Failed { output, to, why } => {
                self.failed(output, to, why);
                None
            }
            Effect::Countdown { output, left, of } => {
                self.countdown(&output, left, of);
                None
            }
            Effect::CloseCountdown => {
                if let Some(layer) = self.countdown.borrow_mut().take() {
                    layer.close();
                }
                None
            }
            // A timer belongs to the window it is set in, and dies when that
            // window closes. So feed() only ever runs in the root's context
            // (the inbox's, and its timers'), never in an overlay's handler:
            // the overlays send what they hear through the inbox
            // (Msg::Overlay). A timer set here outlives every layer.
            Effect::Start {
                timer,
                after,
                generation,
            } => {
                let daemon = self.clone();
                set_timeout(after, move || {
                    daemon.feed(Event::Fired { generation, timer })
                });
                None
            }
            Effect::Note(text) => {
                log(format_args!("{text}"));
                None
            }
        }
    }

    /// Starts a frame of every output, all at once, each on a thread of its
    /// own, which sends it in as [`Msg::Frame`]. The machine's capture timer
    /// bounds the wait; each thread gives up on its own a second before.
    fn capture(&self, generation: u64) -> Option<Event> {
        let outputs = anti_webui::shell::outputs();
        let order: Vec<String> = outputs.iter().map(|o| o.name.clone()).collect();
        if outputs.is_empty() {
            *self.capturing.borrow_mut() = None;
            log(format_args!("no outputs to capture"));
            return Some(Event::Captured {
                generation,
                outputs: Vec::new(),
            });
        }
        log(format_args!("capturing {}", order.join(", ")));
        *self.capturing.borrow_mut() = Some(Capturing {
            generation,
            left: order.len(),
            order,
            frames: Vec::new(),
        });
        for output in outputs {
            let (inbox, failed) = (self.inbox().clone(), self.inbox().clone());
            let name = output.name.clone();
            let start = Instant::now();
            let spawned = std::thread::Builder::new()
                .name("minkashot-capture".into())
                .spawn(move || {
                    let frame = grab::picture(&output, &output.name, grab::PICTURE_TIMEOUT)
                        .map_err(|f| f.to_string());
                    let took = start.elapsed();
                    inbox.send(Msg::Frame {
                        generation,
                        output,
                        frame,
                        took,
                    });
                });
            if let Err(e) = spawned {
                failed.send(Msg::Frame {
                    generation,
                    output: Output {
                        name,
                        ..Default::default()
                    },
                    frame: Err(format!("cannot start its capture thread: {e}")),
                    took: Duration::ZERO,
                });
            }
        }
        None
    }

    /// A frame came, or an output gave none, which leaves it out. Once every
    /// output has answered, the machine hears which gave one.
    fn framed(
        self: &Rc<Self>,
        generation: u64,
        output: Output,
        frame: Result<Arc<Bitmap>, String>,
        took: Duration,
    ) {
        let mut capturing = self.capturing.borrow_mut();
        // For a capture since replaced by another.
        let Some(c) = capturing.as_mut().filter(|c| c.generation == generation) else {
            return;
        };
        // The arm it was for is over: its frames go.
        if self.machine.borrow().generation() != generation {
            *capturing = None;
            return;
        }
        match frame {
            Ok(frame) => {
                log(format_args!(
                    "frame of {}: {}x{} in {:.1} ms, {}",
                    output.name,
                    frame.width(),
                    frame.height(),
                    ms(took),
                    self.after_arm(Instant::now())
                ));
                c.frames.push((output, frame));
            }
            Err(why) => log(format_args!(
                "no frame of {} after {:.1} ms, so it is left out: {why}",
                output.name,
                ms(took)
            )),
        }
        c.left -= 1;
        if c.left > 0 {
            return;
        }
        let mut frames = std::mem::take(&mut c.frames);
        frames.sort_by_key(|(o, _)| c.order.iter().position(|n| *n == o.name));
        *capturing = None;
        drop(capturing);
        let outputs = frames.iter().map(|(o, _)| o.name.clone()).collect();
        *self.frames.borrow_mut() = frames;
        self.feed(Event::Captured {
            generation,
            outputs,
        });
    }

    /// A layer over each output just captured, showing its frame, the
    /// keyboard on `keyboard`'s. `outputs` names the same outputs, in the
    /// same order. Each has the windows of the arm to pick from.
    fn open_overlays(&self, outputs: &[String], keyboard: &str) -> Option<Event> {
        let frames = std::mem::take(&mut *self.frames.borrow_mut());
        // An output gone since its frame came would get its layer anyway, on
        // whichever output the compositor chose. The machine still counts it
        // among the overlays, and may give it the keyboard, so the arm ends.
        let now = anti_webui::shell::outputs();
        if let Some((gone, _)) = frames
            .iter()
            .find(|(o, _)| !now.iter().any(|n| n.name == o.name))
        {
            log(format_args!(
                "{} went away after its frame came; cancelling",
                gone.name
            ));
            return Some(Event::Cancel);
        }
        let windows = self.machine.borrow().windows().to_vec();
        let up = frames
            .into_iter()
            .map(|(output, frame)| {
                let region = Rc::new(Cell::new(None));
                let frozen = Frozen {
                    output: output.name.clone(),
                    position: (output.position.x as f64, output.position.y as f64),
                    frame: frame.clone(),
                    windows: windows.clone(),
                };
                Overlay {
                    layer: self.open_layer(
                        &output,
                        frozen,
                        region.clone(),
                        output.name == keyboard,
                    ),
                    logical: (output.logical_size.width, output.logical_size.height),
                    scale: output.scale,
                    output: output.name,
                    frame,
                    region,
                }
            })
            .collect();
        *self.overlays.borrow_mut() = up;
        log(format_args!(
            "opening the overlays on {}, the keyboard on {keyboard}, {}",
            outputs.join(", "),
            self.after_arm(Instant::now())
        ));
        None
    }

    fn open_layer(
        &self,
        output: &Output,
        frozen: Frozen,
        region: Rc<Cell<Option<Rect>>>,
        keyboard: bool,
    ) -> LayerHandle {
        let options = LayerOptions::new()
            .layer(Layer::Overlay)
            // All of the output: the Minka config blurs what is behind a
            // layer that covers less (minka/rendering.rs).
            .anchor(Anchor::ALL)
            .exclusive(Exclusive::Ignore)
            .keyboard(keyboard_for(keyboard))
            .output(output)
            .namespace(NAME);
        let (name, inbox, shared) = (output.name.clone(), self.inbox().clone(), self.shared);
        open_layer(options, move || {
            let tell: Rc<dyn Fn(Told)> = Rc::new(move |told| inbox.send(Msg::Overlay(told)));
            // Gone for whatever reason: closed here, or it could not open.
            let window = current_window().id();
            let closed = tell.clone();
            on_cleanup(move || closed(Told::Closed(window)));
            // The first frame, and the next one: paced by the compositor's
            // frame callback once the first was committed, or by a retry if
            // presenting it failed.
            let (drawn, output) = (tell.clone(), name.clone());
            request_frame(move |_| {
                let at = Instant::now();
                request_frame(move |_| {
                    drawn(Told::Drawn {
                        output,
                        drawn: at,
                        next: Instant::now(),
                    })
                });
            });
            overlay::view(frozen, shared, region, tell)
        })
    }

    /// Gives the keyboard to the overlay of `output`, and takes it from the
    /// others. The new one takes it first, so there is no moment when no
    /// overlay holds it and it goes back to a window under the frames.
    fn hand_keyboard(&self, output: &str) {
        let up = self.overlays.borrow();
        let Some(to) = up.iter().find(|o| o.output == output) else {
            log(format_args!(
                "no overlay on {output} to give the keyboard to"
            ));
            return;
        };
        to.layer.set_keyboard(keyboard_for(true));
        for o in up.iter().filter(|o| o.output != output) {
            o.layer.set_keyboard(keyboard_for(false));
        }
        log(format_args!(
            "the keyboard to {output}, where the pointer is"
        ));
    }

    /// Closes every overlay, and lets go of the frames. Each layer lets go of
    /// its own copy, and its GPU memory, as it closes. A failure said on
    /// them goes too.
    fn close_overlays(&self) {
        self.shared.failure.set(None);
        let up = std::mem::take(&mut *self.overlays.borrow_mut());
        for o in &up {
            o.layer.close();
        }
        let names: Vec<&str> = up.iter().map(|o| o.output.as_str()).collect();
        if !names.is_empty() {
            log(format_args!("closing the overlays on {}", names.join(", ")));
        }
    }

    /// Writes the frame of `output`, its region or all of it, as `to` says,
    /// with the frames still up, as the QML did, and gives how it went.
    fn save(&self, output: &str, to: To, generation: u64) -> Option<Event> {
        let failed = |why: String| {
            log(format_args!("could not {} {output}: {why}", verb(to)));
            Some(Event::Saved {
                generation,
                failure: Some(why),
            })
        };
        let found = self
            .overlays
            .borrow()
            .iter()
            .find(|o| o.output == output)
            .map(|o| (o.frame.clone(), o.region.get(), o.logical, o.scale));
        let Some((frame, region, logical, scale)) = found else {
            return failed(format!("there is no frame of {output}"));
        };
        let size = (frame.width(), frame.height());
        // The region is kept on the output, so this is never empty.
        let Some(crop) = geometry::crop(region.as_ref(), logical, scale, size) else {
            return failed(format!("{region:?} is not on {output}"));
        };
        let target = match to {
            To::File => match cli::target(None) {
                Ok(target) => Some(target),
                Err(why) => return failed(why.to_string()),
            },
            To::Clipboard => None,
        };
        let name = format!(
            "{output} ({}x{} at {},{})",
            crop.width, crop.height, crop.x, crop.y
        );
        let start = Instant::now();
        let done = match &target {
            Some(target) => cli::write(&frame, crop, target)
                .map(|path| format!("as {}", path.unwrap_or_default().display()))
                .map_err(|f| f.to_string()),
            None => copy(&frame, crop)
                .map(|bytes| format!("to the clipboard, {} KiB of PNG", bytes / 1024)),
        };
        let took = ms(start.elapsed());
        match &done {
            Ok(how) => log(format_args!("{} {name} {how} in {took:.1} ms", past(to))),
            Err(why) => log(format_args!(
                "could not {} {name}, after {took:.1} ms: {why}",
                verb(to)
            )),
        }
        Some(Event::Saved {
            generation,
            failure: done.err(),
        })
    }

    /// Says on the overlay of `output` that its save or copy failed. The
    /// overlays stay as they were.
    fn failed(&self, output: String, to: To, why: String) {
        let text = format!("Not {}: {why}", past(to));
        self.shared
            .failure
            .set(Some(overlay::Failure { output, text }));
    }

    /// Shows the countdown at `left` of `of` seconds on `output`, opening
    /// its layer the first time: over everything, every click going through
    /// it, near the top of the output, as the QML pill was.
    fn countdown(&self, output: &str, left: u8, of: u8) {
        self.left.set(left);
        if self.countdown.borrow().is_some() {
            return;
        }
        let Some(on) = anti_webui::shell::outputs()
            .into_iter()
            .find(|o| o.name == output)
        else {
            log(format_args!(
                "no output {output:?} to show the countdown on; counting down without it"
            ));
            return;
        };
        let options = LayerOptions::overlay()
            .anchor(Anchor::TOP)
            .margin(Margin {
                top: countdown::TOP,
                ..Margin::default()
            })
            .exclusive(Exclusive::Ignore)
            .output(&on)
            .namespace("minkashot-countdown");
        let left = self.left;
        let layer = open_layer(options, move || countdown::view(left, of));
        *self.countdown.borrow_mut() = Some(layer);
        log(format_args!("counting down {of} s on {output}"));
    }

    /// What an overlay told, on the root's side.
    fn told(self: &Rc<Self>, told: Told) {
        match told {
            // A retry taken is a new arm: its timings count from here.
            Told::Event(Event::Retry) => {
                let generation = self.machine.borrow().generation();
                self.feed(Event::Retry);
                if self.machine.borrow().generation() != generation {
                    self.armed_at.set(Some((Instant::now(), "Retry")));
                }
            }
            Told::Event(event) => self.feed(event),
            Told::Drawn {
                output,
                drawn,
                next,
            } => log(format_args!(
                "{output}: first frame drawn {}, the next started {} (about when the first \
                 was on screen, unless a \"presenting our own buffers failed\" line came)",
                self.after_arm(drawn),
                self.after_arm(next)
            )),
            // One the daemon closed is no longer listed; one still listed
            // closed by itself.
            Told::Closed(window) => {
                let gone = {
                    let mut up = self.overlays.borrow_mut();
                    let at = up.iter().position(|o| o.layer.id() == window);
                    at.map(|at| up.remove(at))
                };
                if let Some(o) = gone {
                    log(format_args!(
                        "the overlay on {} closed by itself (it could not open, or failed); \
                         cancelling",
                        o.output
                    ));
                    self.feed(Event::Cancel);
                }
            }
        }
    }
}

/// What a save or a copy is, for the log.
fn verb(to: To) -> &'static str {
    match to {
        To::File => "save",
        To::Clipboard => "copy",
    }
}

fn past(to: To) -> &'static str {
    match to {
        To::File => "saved",
        To::Clipboard => "copied",
    }
}

/// Puts `crop` of `frame` on the clipboard as a PNG, and gives its size.
fn copy(frame: &Bitmap, crop: Crop) -> Result<usize, String> {
    let png = cli::png(frame, crop).map_err(|f| f.to_string())?;
    // Write-only: the daemon never sees what anything else copies.
    let clipboard = anti_webui::shell::clipboard_writer();
    if !clipboard.available() {
        return Err(
            "there is no clipboard to copy to (the compositor offers no data control)".into(),
        );
    }
    let size = png.len();
    clipboard.set_data("image/png", png);
    Ok(size)
}

/// The keyboard an overlay is opened with or given: all of it for the
/// overlay the pointer is on, and on demand for the others. Not none: the
/// compositor's hit test passes over a layer that covers the output and
/// takes no keyboard, and the pointer would go through to MinkaShell's dock
/// and side column under the frames.
fn keyboard_for(active: bool) -> Keyboard {
    if active {
        Keyboard::Exclusive
    } else {
        Keyboard::OnDemand
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_overlay_catches_the_pointer() {
        assert_eq!(keyboard_for(true), Keyboard::Exclusive);
        assert_eq!(keyboard_for(false), Keyboard::OnDemand);
    }

    #[test]
    fn each_command_is_its_word() {
        for command in [Command::Interactive, Command::Cancel, Command::Status] {
            assert_eq!(Command::from_word(command.word()), Some(command));
        }
        assert_eq!(Command::from_word("Status"), None);
        assert_eq!(Command::from_word("full"), None);
    }

    #[test]
    fn a_stamp_has_milliseconds() {
        let at = UNIX_EPOCH + Duration::from_millis(1_764_000_000_007);
        let s = stamp(at);
        assert_eq!(s.len(), "12:34:56.007".len(), "{s}");
        assert!(s.ends_with(".007"), "{s}");
    }
}
