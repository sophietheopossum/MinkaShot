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
//! So far it opens no surface: the window list and the timers are carried
//! out, and the capture, the overlays and the countdown are only logged. A
//! capture gives no frames, so each arm ends idle again with a note.

use anti_webui::core::animation::set_timeout;
use anti_webui::core::channel::ui_channel;
use anti_webui::shell::{LocalTime, Shell};
use minka_instance::{Claim, Instance, RequestError};
use minka_ipc::{IpcClient, IpcEvent};
use serde_json::{Value, json};
use std::cell::{Cell, RefCell};
use std::collections::VecDeque;
use std::fmt;
use std::rc::Rc;
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use crate::cli::Failure;
use crate::state::{Effect, Event, Layout, Machine};
use crate::window;

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
}

/// What the daemon keeps on the UI thread.
struct Daemon {
    machine: RefCell<Machine>,
    ipc: Option<Arc<IpcClient>>,
    connected: Cell<bool>,
    /// The `workspaces.get` in flight: its id, the generation it answers,
    /// and when it was asked.
    fetch: Cell<Option<(u64, u64, Instant)>>,
}

/// Runs on the UI thread, inside the shell's root: the inbox and its
/// threads.
fn root(instance: Instance) {
    let ipc = IpcClient::spawn();
    let daemon = Rc::new(Daemon {
        machine: RefCell::new(Machine::new(Instant::now())),
        ipc: ipc.as_ref().ok().map(|(client, _)| client.clone()),
        connected: Cell::new(false),
        fetch: Cell::new(None),
    });
    let inbox = ui_channel(move |msg: Msg| daemon.handle(msg));

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
                self.interactive();
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
                    Some(Command::Interactive) => self.interactive(),
                    Some(Command::Cancel) => self.cancel(),
                    Some(Command::Status) => self.status(),
                    None => format!("unknown request {:?}", request.line()),
                };
                request.answer(&answer);
            }
        }
    }

    /// What the machine is doing, and its generation. Every change of phase
    /// is a new generation, so an unchanged one says an event was not taken.
    fn seen(&self) -> (String, u64) {
        let machine = self.machine.borrow();
        (machine.describe(), machine.generation())
    }

    /// Print, or `minkashot interactive`; the answer to the latter. Whether
    /// it arms is the machine's rule.
    fn interactive(self: &Rc<Self>) -> String {
        let (before, generation) = self.seen();
        self.feed(Event::Interactive);
        let (now, after) = self.seen();
        if after == generation {
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
            "{} since {}; delay {} s; compositor IPC {ipc}; pid {}",
            machine.describe(),
            stamp(since),
            machine.delay(),
            std::process::id()
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
            // Not built yet: no frames, so the arm ends here.
            Effect::Capture { generation } => {
                log(format_args!(
                    "capture every output: not built yet, so no frames"
                ));
                Some(Event::Captured {
                    generation,
                    outputs: Vec::new(),
                })
            }
            // The layers come with the overlay; until then they are logged.
            effect @ (Effect::OpenOverlays { .. }
            | Effect::CloseOverlays
            | Effect::Countdown { .. }
            | Effect::CloseCountdown) => {
                log(format_args!("not built yet: {effect:?}"));
                None
            }
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
}

#[cfg(test)]
mod tests {
    use super::*;

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
