//! What the daemon is doing, as the QML ShotState did it, with no I/O:
//! events go in with the time, and effects come out for the daemon to carry
//! out. Every output is captured first, and the overlays open only once the
//! frames are in hand.
//!
//! Each phase has a generation of its own. What a phase waits for (the
//! window list, the frames, a timer) comes back with the generation it was
//! asked in, and counts only while that phase lasts: an answer for an arm
//! since cancelled, retried or given up on is ignored.
//!
//! Nothing is captured while a MinkaShot layer is open, nor within
//! [`SETTLE`] of one closing: the compositor must have drawn the screen
//! without them first, or the old frozen frame ends up in the new one.
//! [`Machine::capture`] is the one place a capture is asked for. It is
//! reached only from phases with no layer open, and it waits out what is
//! left of the settle.
//!
//! A shot is saved (Enter) or copied (the toolbar) with the frames still
//! up: they close once it is done. One that fails puts the frames back as
//! they were, with what went wrong, so the shot is not lost.

use std::time::{Duration, Instant};

use crate::window::WindowRect;

/// How long the screen is given to be drawn without MinkaShot's layers
/// before a capture.
pub const SETTLE: Duration = Duration::from_millis(250);
/// One step of the countdown before a retry.
pub const TICK: Duration = Duration::from_secs(1);
/// How long the compositor may take to give the window list. Without it
/// the arm goes on, with no windows to pick.
pub const FETCH_WAIT: Duration = Duration::from_secs(1);
/// How long the frames may take: the capture's own bound
/// (`grab::PICTURE_TIMEOUT`, 6 s) and a second more.
pub const CAPTURE_WAIT: Duration = Duration::from_secs(7);
/// The longest delay before a retry, in seconds.
pub const MAX_DELAY: u8 = 10;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Phase {
    Idle,
    /// Waiting for the window list and the output under the pointer.
    Fetching,
    /// Waiting for a frame of every output.
    Capturing,
    /// The frozen frames are up.
    Shown,
    /// The frames are up, and the active output's shot is being written
    /// there.
    Saving(To),
    /// Before a retry, with this many seconds left.
    Counting(u8),
    /// Waiting for the screen to be drawn without MinkaShot's layers.
    Settling,
}

/// Where a shot goes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum To {
    /// A PNG under the default name (Enter).
    File,
    /// The clipboard, as `image/png` (the toolbar's copy).
    Clipboard,
}

/// A wait that ends in [`Event::Fired`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Timer {
    /// The window list is late.
    Fetch,
    /// The frames are late.
    Capture,
    /// A second of the countdown.
    Tick,
    /// The settle is over.
    Settle,
}

/// What `workspaces.get` said.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Layout {
    /// The output under the pointer (`currentMonitor`).
    pub current: Option<String>,
    /// The windows, top first.
    pub rects: Vec<WindowRect>,
}

#[derive(Debug, PartialEq)]
pub enum Event {
    /// Print, or `minkashot interactive`: arm, when idle; during the
    /// countdown, freeze now, as the QML did.
    Interactive,
    /// Esc, or `minkashot cancel`: back to idle from anywhere. Nothing else
    /// closes the frames but Enter, as in the QML.
    Cancel,
    /// The pointer is on this output's overlay, which makes it the active
    /// output, with the keyboard.
    Pointer(String),
    /// Enter: save the active output's frame, its region or all of it, and
    /// close once it is saved.
    Save,
    /// The toolbar's copy: the same onto the clipboard.
    Copy,
    /// How the save or the copy went: `None` when it went through, else
    /// why not.
    Saved {
        generation: u64,
        failure: Option<String>,
    },
    /// The toolbar's retry: close the overlays, count down the delay and
    /// freeze again.
    Retry,
    /// The toolbar's window toggle.
    TogglePick,
    /// A window was picked, which is now the region: picking is over. Not
    /// a toggle, so a second pick before this is heard cannot turn it on
    /// again.
    Picked,
    /// The toolbar's delay stepper: the seconds before a retry, kept to
    /// 0..=[`MAX_DELAY`].
    Delay(i32),
    /// The answer to [`Effect::FetchWindows`]; `None` when it failed.
    Windows {
        generation: u64,
        layout: Option<Layout>,
    },
    /// The answer to [`Effect::Capture`]: the outputs that gave a frame.
    Captured {
        generation: u64,
        outputs: Vec<String>,
    },
    /// A timer from [`Effect::Start`] ran out.
    Fired { generation: u64, timer: Timer },
}

#[derive(Debug, PartialEq)]
pub enum Effect {
    /// Ask `workspaces.get`, and answer with [`Event::Windows`].
    FetchWindows {
        generation: u64,
    },
    /// Take a frame of every output, and answer with [`Event::Captured`].
    Capture {
        generation: u64,
    },
    /// Open an overlay showing its frame on each of `outputs`, with the
    /// keyboard on `keyboard`'s.
    OpenOverlays {
        outputs: Vec<String>,
        keyboard: String,
    },
    CloseOverlays,
    /// Give the keyboard to the overlay of `output`, from the one that has
    /// it: the pointer came onto it.
    Keyboard {
        output: String,
    },
    /// Write the frame of `output`, its region or all of it, as `to` says,
    /// and answer with [`Event::Saved`]. The frames stay up meanwhile.
    Save {
        output: String,
        to: To,
        generation: u64,
    },
    /// Say on the overlay of `output`, the active one, that the save or the
    /// copy failed, and why.
    Failed {
        output: String,
        to: To,
        why: String,
    },
    /// Show the countdown at `left` seconds of `of` on `output`, the active
    /// one, opening its layer if it is not open.
    Countdown {
        output: String,
        left: u8,
        of: u8,
    },
    CloseCountdown,
    /// Send [`Event::Fired`] after `after`.
    Start {
        timer: Timer,
        after: Duration,
        generation: u64,
    },
    /// A line for the log.
    Note(String),
}

#[derive(Debug)]
pub struct Machine {
    phase: Phase,
    since: Instant,
    generation: u64,
    /// Seconds before a retry. Kept for the daemon's life, as in the QML.
    delay: u8,
    /// The seconds the countdown on now began at.
    from: u8,
    pick: bool,
    /// The active output: the one under the pointer at the arm, and while
    /// the frames are up, the one the pointer was last on. Enter saves it.
    active: Option<String>,
    /// The windows at the arm, top first.
    rects: Vec<WindowRect>,
    /// The outputs whose overlays are open.
    overlays: Vec<String>,
    /// Whether the countdown's layer is open.
    countdown: bool,
    /// When a MinkaShot layer last closed.
    closed_at: Option<Instant>,
}

impl Machine {
    pub fn new(now: Instant) -> Self {
        Machine {
            phase: Phase::Idle,
            since: now,
            generation: 0,
            delay: 0,
            from: 0,
            pick: false,
            active: None,
            rects: Vec::new(),
            overlays: Vec::new(),
            countdown: false,
            closed_at: None,
        }
    }

    /// Rises with every change of phase, and only then.
    pub fn generation(&self) -> u64 {
        self.generation
    }

    /// When the phase began.
    pub fn since(&self) -> Instant {
        self.since
    }

    pub fn delay(&self) -> u8 {
        self.delay
    }

    /// Whether a press on the frames picks a window.
    pub fn pick(&self) -> bool {
        self.pick
    }

    /// Whether the frozen frames are up: shown, or being saved from.
    fn up(&self) -> bool {
        matches!(self.phase, Phase::Shown | Phase::Saving(_))
    }

    /// The windows at the arm, top first.
    pub fn windows(&self) -> &[WindowRect] {
        &self.rects
    }

    /// The phase, in words.
    pub fn describe(&self) -> String {
        match self.phase {
            Phase::Idle => "idle".into(),
            Phase::Fetching => "arming: asking the compositor for its windows".into(),
            Phase::Capturing => "arming: capturing every output".into(),
            Phase::Shown => {
                let pick = if self.pick { ", picking a window" } else { "" };
                format!("shown on {}{pick}", self.overlays.join(", "))
            }
            Phase::Saving(to) => {
                let doing = match to {
                    To::File => "saving",
                    To::Clipboard => "copying",
                };
                let active = self.active.as_deref().unwrap_or("no output");
                format!("{doing} {active}, shown on {}", self.overlays.join(", "))
            }
            Phase::Counting(left) => format!("counting down to a retry, {left} s left"),
            Phase::Settling => "waiting for the screen to settle before a capture".into(),
        }
    }

    pub fn step(&mut self, event: Event, now: Instant) -> Vec<Effect> {
        let mut out = Vec::new();
        match event {
            // Print while armed or settling does nothing.
            Event::Interactive if self.phase == Phase::Idle => self.fetch(now, &mut out),
            // During the countdown it freezes now, the rest of the countdown
            // skipped, though not before the settle.
            Event::Interactive if matches!(self.phase, Phase::Counting(_)) => {
                self.close_layers(now, &mut out);
                self.settle(SETTLE, now, &mut out);
            }
            Event::Cancel if self.phase != Phase::Idle => self.idle(now, &mut out),
            Event::Retry if self.phase == Phase::Shown => {
                self.close_layers(now, &mut out);
                self.pick = false;
                if self.delay > 0 {
                    self.from = self.delay;
                    self.count(self.delay, now, &mut out);
                } else {
                    self.settle(SETTLE, now, &mut out);
                }
            }
            Event::TogglePick if self.phase == Phase::Shown => self.pick = !self.pick,
            Event::Picked => self.pick = false,
            // The keyboard follows the pointer, so Esc and Enter are always
            // for the output it is on, a save going on or not.
            Event::Pointer(output)
                if self.up()
                    && self.overlays.contains(&output)
                    && self.active.as_ref() != Some(&output) =>
            {
                self.active = Some(output.clone());
                out.push(Effect::Keyboard { output });
            }
            Event::Save if self.phase == Phase::Shown => self.save(To::File, now, &mut out),
            Event::Copy if self.phase == Phase::Shown => self.save(To::Clipboard, now, &mut out),
            Event::Saved {
                generation,
                failure,
            } if generation == self.generation => {
                if let Phase::Saving(to) = self.phase {
                    match failure {
                        None => self.idle(now, &mut out),
                        // The frames as they were, to try again, copy or let go.
                        Some(why) => {
                            self.enter(Phase::Shown, now);
                            out.push(Effect::Failed {
                                output: self.active.clone().unwrap_or_default(),
                                to,
                                why,
                            });
                        }
                    }
                }
            }
            Event::Delay(seconds) => self.delay = seconds.clamp(0, MAX_DELAY.into()) as u8,
            Event::Windows { generation, layout }
                if self.is_current(generation, Phase::Fetching) =>
            {
                match layout {
                    Some(layout) => {
                        // A retry keeps the output it had when none is named.
                        if layout.current.is_some() {
                            self.active = layout.current;
                        }
                        self.rects = layout.rects;
                    }
                    None => self.rects.clear(),
                }
                self.capture(now, &mut out);
            }
            Event::Captured {
                generation,
                outputs,
            } if self.is_current(generation, Phase::Capturing) => {
                if outputs.is_empty() {
                    out.push(Effect::Note("no output gave a frame; idle again".into()));
                    self.idle(now, &mut out);
                } else {
                    let keyboard = match &self.active {
                        Some(a) if outputs.contains(a) => a.clone(),
                        _ => outputs[0].clone(),
                    };
                    self.active = Some(keyboard.clone());
                    self.overlays = outputs.clone();
                    self.enter(Phase::Shown, now);
                    out.push(Effect::OpenOverlays { outputs, keyboard });
                }
            }
            Event::Fired { generation, timer } if generation == self.generation => {
                match (timer, self.phase) {
                    (Timer::Fetch, Phase::Fetching) => {
                        out.push(Effect::Note(format!(
                            "no window list from the compositor within {} s; going on without it",
                            FETCH_WAIT.as_secs()
                        )));
                        self.rects.clear();
                        self.capture(now, &mut out);
                    }
                    (Timer::Capture, Phase::Capturing) => {
                        out.push(Effect::Note(format!(
                            "no frames within {} s; idle again",
                            CAPTURE_WAIT.as_secs()
                        )));
                        self.idle(now, &mut out);
                    }
                    (Timer::Tick, Phase::Counting(left)) if left > 1 => {
                        self.count(left - 1, now, &mut out)
                    }
                    (Timer::Tick, Phase::Counting(_)) => {
                        self.close_layers(now, &mut out);
                        self.settle(SETTLE, now, &mut out);
                    }
                    (Timer::Settle, Phase::Settling) => self.fetch(now, &mut out),
                    _ => {}
                }
            }
            // Anything else is for another phase, or a phase now over.
            _ => {}
        }
        out
    }

    /// Whether what came back with `generation` is for `phase`, and that
    /// phase is still on.
    fn is_current(&self, generation: u64, phase: Phase) -> bool {
        self.generation == generation && self.phase == phase
    }

    /// Moves to `phase`, a new generation.
    fn enter(&mut self, phase: Phase, now: Instant) {
        self.phase = phase;
        self.since = now;
        self.generation += 1;
    }

    fn start(&self, timer: Timer, after: Duration, out: &mut Vec<Effect>) {
        out.push(Effect::Start {
            timer,
            after,
            generation: self.generation,
        });
    }

    fn fetch(&mut self, now: Instant, out: &mut Vec<Effect>) {
        self.enter(Phase::Fetching, now);
        out.push(Effect::FetchWindows {
            generation: self.generation,
        });
        self.start(Timer::Fetch, FETCH_WAIT, out);
    }

    /// Asks for a frame of every output, unless a MinkaShot layer closed
    /// less than [`SETTLE`] ago. Then it waits the rest out, and asks for the
    /// window list again after, as a retry does.
    fn capture(&mut self, now: Instant, out: &mut Vec<Effect>) {
        if let Some(left) = self.settle_left(now) {
            self.settle(left, now, out);
            return;
        }
        self.enter(Phase::Capturing, now);
        out.push(Effect::Capture {
            generation: self.generation,
        });
        self.start(Timer::Capture, CAPTURE_WAIT, out);
    }

    /// How long until no MinkaShot layer can be in a capture; `None` when
    /// none can now. Layers are open only while shown or counting down, and
    /// no capture is asked for in either.
    fn settle_left(&self, now: Instant) -> Option<Duration> {
        let ready = self.closed_at? + SETTLE;
        ready.checked_duration_since(now).filter(|d| !d.is_zero())
    }

    fn settle(&mut self, after: Duration, now: Instant, out: &mut Vec<Effect>) {
        self.enter(Phase::Settling, now);
        self.start(Timer::Settle, after, out);
    }

    /// Writes the active output's shot as `to` says, the frames left up
    /// until it is done.
    fn save(&mut self, to: To, now: Instant, out: &mut Vec<Effect>) {
        // Shown has an active output: it is set as the frames come.
        let Some(output) = self.active.clone() else {
            return;
        };
        self.enter(Phase::Saving(to), now);
        out.push(Effect::Save {
            output,
            to,
            generation: self.generation,
        });
    }

    fn count(&mut self, left: u8, now: Instant, out: &mut Vec<Effect>) {
        self.countdown = true;
        self.enter(Phase::Counting(left), now);
        out.push(Effect::Countdown {
            // Named since the frames were shown, which a countdown follows.
            output: self.active.clone().unwrap_or_default(),
            left,
            of: self.from,
        });
        self.start(Timer::Tick, TICK, out);
    }

    fn close_layers(&mut self, now: Instant, out: &mut Vec<Effect>) {
        if !self.overlays.is_empty() {
            self.overlays.clear();
            self.closed_at = Some(now);
            out.push(Effect::CloseOverlays);
        }
        if self.countdown {
            self.countdown = false;
            self.closed_at = Some(now);
            out.push(Effect::CloseCountdown);
        }
    }

    /// Back to idle: every layer closed, and everything but the delay
    /// forgotten.
    fn idle(&mut self, now: Instant, out: &mut Vec<Effect>) {
        self.close_layers(now, out);
        self.pick = false;
        self.active = None;
        self.rects.clear();
        self.enter(Phase::Idle, now);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::geometry::Rect;

    fn ms(n: u64) -> Duration {
        Duration::from_millis(n)
    }

    fn window(title: &str) -> WindowRect {
        WindowRect {
            title: title.into(),
            monitor: "eDP-1".into(),
            rect: Rect {
                x: 14.0,
                y: 14.0,
                width: 100.0,
                height: 100.0,
            },
            last_focused_at: 1.0,
        }
    }

    fn layout(current: &str, titles: &[&str]) -> Option<Layout> {
        Some(Layout {
            current: Some(current.into()),
            rects: titles.iter().map(|t| window(t)).collect(),
        })
    }

    fn outputs(names: &[&str]) -> Vec<String> {
        names.iter().map(|n| n.to_string()).collect()
    }

    /// The answer to the window list asked for in this phase.
    fn windows(m: &mut Machine, layout: Option<Layout>, at: Instant) -> Vec<Effect> {
        let generation = m.generation;
        m.step(Event::Windows { generation, layout }, at)
    }

    fn captured(m: &mut Machine, names: &[&str], at: Instant) -> Vec<Effect> {
        let generation = m.generation;
        m.step(
            Event::Captured {
                generation,
                outputs: outputs(names),
            },
            at,
        )
    }

    fn fire(m: &mut Machine, timer: Timer, at: Instant) -> Vec<Effect> {
        let generation = m.generation;
        m.step(Event::Fired { generation, timer }, at)
    }

    /// The answer to the save asked for in this phase.
    fn saved(m: &mut Machine, failure: Option<&str>, at: Instant) -> Vec<Effect> {
        let generation = m.generation;
        let failure = failure.map(String::from);
        m.step(
            Event::Saved {
                generation,
                failure,
            },
            at,
        )
    }

    /// Print, the window list with the pointer on eDP-1, and frames of
    /// DP-1 and eDP-1: shown.
    fn shown(m: &mut Machine, at: Instant) {
        m.step(Event::Interactive, at);
        windows(m, layout("eDP-1", &["kitty"]), at);
        captured(m, &["DP-1", "eDP-1"], at);
        assert_eq!(m.phase, Phase::Shown);
    }

    fn captures(effects: &[Effect]) -> bool {
        effects.iter().any(|e| matches!(e, Effect::Capture { .. }))
    }

    #[test]
    fn print_fetches_the_windows_then_captures_then_shows() {
        let t = Instant::now();
        let mut m = Machine::new(t);
        assert_eq!(
            m.step(Event::Interactive, t),
            [
                Effect::FetchWindows { generation: 1 },
                Effect::Start {
                    timer: Timer::Fetch,
                    after: FETCH_WAIT,
                    generation: 1
                },
            ]
        );
        assert_eq!(m.phase, Phase::Fetching);
        assert_eq!(
            windows(&mut m, layout("eDP-1", &["kitty", "floorp"]), t + ms(5)),
            [
                Effect::Capture { generation: 2 },
                Effect::Start {
                    timer: Timer::Capture,
                    after: CAPTURE_WAIT,
                    generation: 2
                },
            ]
        );
        assert_eq!(m.phase, Phase::Capturing);
        assert_eq!(m.rects, [window("kitty"), window("floorp")]);
        // The pointer's output has the keyboard, not the first output.
        let effects = captured(&mut m, &["DP-1", "eDP-1"], t + ms(90));
        assert_eq!(
            effects,
            [Effect::OpenOverlays {
                outputs: outputs(&["DP-1", "eDP-1"]),
                keyboard: "eDP-1".into(),
            }]
        );
        assert_eq!(m.phase, Phase::Shown);
        assert_eq!(m.since(), t + ms(90));
        assert_eq!(m.describe(), "shown on DP-1, eDP-1");
        // The window rects stay what they were at the arm.
        assert_eq!(m.rects, [window("kitty"), window("floorp")]);
    }

    #[test]
    fn the_keyboard_goes_to_the_first_frame_when_the_pointer_has_none() {
        let t = Instant::now();
        let mut m = Machine::new(t);
        m.step(Event::Interactive, t);
        windows(&mut m, layout("HDMI-A-3", &[]), t);
        let opened = captured(&mut m, &["DP-1", "eDP-1"], t);
        assert!(
            matches!(&opened[..], [Effect::OpenOverlays { keyboard, .. }] if keyboard == "DP-1"),
            "{opened:?}"
        );
        assert_eq!(m.active.as_deref(), Some("DP-1"));

        let mut m = Machine::new(t);
        m.step(Event::Interactive, t);
        windows(&mut m, None, t);
        assert_eq!(m.active, None);
        let opened = captured(&mut m, &["eDP-1"], t);
        assert!(
            matches!(&opened[..], [Effect::OpenOverlays { keyboard, .. }] if keyboard == "eDP-1"),
            "{opened:?}"
        );
    }

    #[test]
    fn no_frames_is_idle_again() {
        let t = Instant::now();
        let mut m = Machine::new(t);
        m.step(Event::Interactive, t);
        windows(&mut m, layout("eDP-1", &["kitty"]), t);
        let effects = captured(&mut m, &[], t + ms(3));
        assert!(matches!(&effects[..], [Effect::Note(_)]), "{effects:?}");
        assert_eq!(m.phase, Phase::Idle);
        assert_eq!(m.since(), t + ms(3));
        assert!(m.rects.is_empty() && m.active.is_none());
        // Print works again at once: no layer was open.
        assert_eq!(
            m.step(Event::Interactive, t + ms(4))[0],
            Effect::FetchWindows {
                generation: m.generation
            }
        );
    }

    #[test]
    fn a_late_window_list_is_given_up_on() {
        let t = Instant::now();
        let mut m = Machine::new(t);
        m.step(Event::Interactive, t);
        let asked = m.generation;
        let effects = fire(&mut m, Timer::Fetch, t + FETCH_WAIT);
        assert!(matches!(effects[0], Effect::Note(_)), "{effects:?}");
        assert!(captures(&effects));
        assert_eq!(m.phase, Phase::Capturing);
        assert!(m.rects.is_empty());
        // The list that comes after all is for a phase that is over.
        let late = m.step(
            Event::Windows {
                generation: asked,
                layout: layout("eDP-1", &["kitty"]),
            },
            t + ms(1200),
        );
        assert_eq!(late, []);
        assert!(m.rects.is_empty());
        assert_eq!(m.phase, Phase::Capturing);
    }

    #[test]
    fn frames_that_never_come_are_given_up_on() {
        let t = Instant::now();
        let mut m = Machine::new(t);
        m.step(Event::Interactive, t);
        windows(&mut m, layout("eDP-1", &[]), t);
        let asked = m.generation;
        let effects = fire(&mut m, Timer::Capture, t + CAPTURE_WAIT);
        assert!(matches!(&effects[..], [Effect::Note(_)]), "{effects:?}");
        assert_eq!(m.phase, Phase::Idle);
        let late = m.step(
            Event::Captured {
                generation: asked,
                outputs: outputs(&["eDP-1"]),
            },
            t + ms(7100),
        );
        assert_eq!(late, []);
        assert_eq!(m.phase, Phase::Idle);
    }

    #[test]
    fn print_while_armed_or_settling_does_nothing() {
        let t = Instant::now();
        let mut m = Machine::new(t);
        m.step(Event::Interactive, t);
        let mut seen = vec![m.phase];
        let mut check = |m: &mut Machine| {
            let (phase, generation, since) = (m.phase, m.generation, m.since);
            assert_eq!(m.step(Event::Interactive, t + ms(500)), [], "{phase:?}");
            assert_eq!((m.phase, m.generation, m.since), (phase, generation, since));
            seen.push(phase);
        };
        check(&mut m);
        windows(&mut m, layout("eDP-1", &[]), t);
        check(&mut m);
        captured(&mut m, &["eDP-1"], t);
        check(&mut m);
        m.step(Event::Copy, t);
        check(&mut m);
        saved(&mut m, Some("no clipboard"), t);
        m.step(Event::Retry, t);
        check(&mut m);
        assert_eq!(
            seen,
            [
                Phase::Fetching,
                Phase::Fetching,
                Phase::Capturing,
                Phase::Shown,
                Phase::Saving(To::Clipboard),
                Phase::Settling
            ]
        );
    }

    #[test]
    fn cancel_from_anywhere_closes_what_is_open_and_forgets() {
        let t = Instant::now();
        let at = t + ms(10);
        // Each way into a phase, and what a cancel there closes.
        type Setup = fn(&mut Machine, Instant);
        let cases: [(Setup, &[Effect]); 6] = [
            (
                |m, t| {
                    m.step(Event::Interactive, t);
                },
                &[],
            ),
            (
                |m, t| {
                    m.step(Event::Interactive, t);
                    windows(m, layout("eDP-1", &["kitty"]), t);
                },
                &[],
            ),
            (|m, t| shown(m, t), &[Effect::CloseOverlays]),
            (
                |m, t| {
                    shown(m, t);
                    m.step(Event::Save, t);
                },
                &[Effect::CloseOverlays],
            ),
            (
                |m, t| {
                    m.step(Event::Delay(3), t);
                    shown(m, t);
                    m.step(Event::Retry, t);
                },
                &[Effect::CloseCountdown],
            ),
            (
                |m, t| {
                    shown(m, t);
                    m.step(Event::Retry, t);
                },
                &[],
            ),
        ];
        for (setup, closes) in cases {
            let mut m = Machine::new(t);
            setup(&mut m, t);
            m.step(Event::TogglePick, t);
            let phase = m.phase;
            assert_eq!(m.step(Event::Cancel, at), closes, "{phase:?}");
            assert_eq!(m.phase, Phase::Idle, "{phase:?}");
            assert_eq!(m.since(), at);
            assert!(!m.pick && m.active.is_none() && m.rects.is_empty());
            assert!(m.overlays.is_empty() && !m.countdown, "{phase:?}");
            // The delay outlives the arm.
            assert_eq!(m.delay(), if phase == Phase::Counting(3) { 3 } else { 0 });
            // Idle already: nothing to do.
            assert_eq!(m.step(Event::Cancel, at), []);
        }
    }

    #[test]
    fn retry_without_a_delay_settles_then_refreshes_and_captures() {
        let t = Instant::now();
        let mut m = Machine::new(t);
        shown(&mut m, t);
        m.step(Event::TogglePick, t);
        assert!(m.pick);
        let r = t + ms(2000);
        assert_eq!(
            m.step(Event::Retry, r),
            [
                Effect::CloseOverlays,
                Effect::Start {
                    timer: Timer::Settle,
                    after: SETTLE,
                    generation: m.generation
                },
            ]
        );
        assert!(!m.pick);
        assert_eq!(m.phase, Phase::Settling);
        let effects = fire(&mut m, Timer::Settle, r + SETTLE);
        assert_eq!(
            effects[0],
            Effect::FetchWindows {
                generation: m.generation
            }
        );
        let effects = windows(&mut m, layout("DP-1", &["moved"]), r + ms(260));
        assert!(captures(&effects), "{effects:?}");
        assert_eq!(m.rects, [window("moved")]);
        assert_eq!(m.active.as_deref(), Some("DP-1"));
        captured(&mut m, &["DP-1", "eDP-1"], r + ms(300));
        assert_eq!(m.phase, Phase::Shown);
    }

    #[test]
    fn a_retry_keeps_its_output_when_the_list_names_none() {
        let t = Instant::now();
        let mut m = Machine::new(t);
        shown(&mut m, t);
        m.step(Event::Retry, t);
        fire(&mut m, Timer::Settle, t + SETTLE);
        windows(&mut m, None, t + SETTLE);
        assert_eq!(m.active.as_deref(), Some("eDP-1"));
        assert!(m.rects.is_empty());
    }

    #[test]
    fn retry_with_a_delay_counts_down_on_its_own_layer() {
        let t = Instant::now();
        let mut m = Machine::new(t);
        m.step(Event::Delay(3), t);
        shown(&mut m, t);
        let tick = |m: &Machine| Effect::Start {
            timer: Timer::Tick,
            after: TICK,
            generation: m.generation,
        };
        let countdown = |left| Effect::Countdown {
            output: "eDP-1".into(),
            left,
            of: 3,
        };
        let effects = m.step(Event::Retry, t);
        assert_eq!(effects, [Effect::CloseOverlays, countdown(3), tick(&m)]);
        assert_eq!(m.describe(), "counting down to a retry, 3 s left");
        let effects = fire(&mut m, Timer::Tick, t + ms(1000));
        assert_eq!(effects, [countdown(2), tick(&m)]);
        // A delay set meanwhile is for the next retry: this one stays of 3.
        m.step(Event::Delay(7), t + ms(1500));
        // A tick that comes late still counts one second.
        let effects = fire(&mut m, Timer::Tick, t + ms(2700));
        assert_eq!(effects, [countdown(1), tick(&m)]);
        let closed = t + ms(3700);
        let effects = fire(&mut m, Timer::Tick, closed);
        assert_eq!(
            effects,
            [
                Effect::CloseCountdown,
                Effect::Start {
                    timer: Timer::Settle,
                    after: SETTLE,
                    generation: m.generation
                }
            ]
        );
        assert_eq!(m.closed_at, Some(closed));
        // A settle that fires late is still a settle.
        let effects = fire(&mut m, Timer::Settle, closed + ms(900));
        assert!(matches!(effects[0], Effect::FetchWindows { .. }));
        let effects = windows(&mut m, layout("eDP-1", &[]), closed + ms(905));
        assert!(captures(&effects));
        // The delay is kept for the next retry.
        captured(&mut m, &["eDP-1"], closed + ms(950));
        let effects = m.step(Event::Retry, closed + ms(990));
        assert!(
            matches!(effects[1], Effect::Countdown { left: 7, of: 7, .. }),
            "{effects:?}"
        );
    }

    /// As in the QML: Print while it counts down freezes at once, the rest
    /// of the countdown skipped, but only once the screen has been drawn
    /// without the countdown's layer.
    #[test]
    fn print_during_the_countdown_freezes_after_the_settle() {
        let t = Instant::now();
        let mut m = Machine::new(t);
        m.step(Event::Delay(5), t);
        shown(&mut m, t);
        m.step(Event::Retry, t);
        fire(&mut m, Timer::Tick, t + ms(1000));
        assert_eq!(m.phase, Phase::Counting(4));
        let counting = m.generation;
        let at = t + ms(1500);
        assert_eq!(
            m.step(Event::Interactive, at),
            [
                Effect::CloseCountdown,
                Effect::Start {
                    timer: Timer::Settle,
                    after: SETTLE,
                    generation: m.generation
                }
            ]
        );
        assert_eq!((m.phase, m.closed_at), (Phase::Settling, Some(at)));
        // The countdown's next second is for a phase that is over.
        let tick = Event::Fired {
            generation: counting,
            timer: Timer::Tick,
        };
        assert_eq!(m.step(tick, t + ms(2000)), []);
        // Print again while it settles: nothing more.
        assert_eq!(m.step(Event::Interactive, at + ms(10)), []);
        let effects = fire(&mut m, Timer::Settle, at + SETTLE);
        assert!(matches!(effects[0], Effect::FetchWindows { .. }));
        let effects = windows(&mut m, layout("eDP-1", &[]), at + SETTLE + ms(5));
        assert!(captures(&effects), "{effects:?}");
        // The delay is kept for the next retry.
        assert_eq!(m.delay(), 5);
    }

    #[test]
    fn timers_after_a_cancel_or_from_an_earlier_arm_are_ignored() {
        let t = Instant::now();
        let mut m = Machine::new(t);
        m.step(Event::Delay(2), t);
        shown(&mut m, t);
        m.step(Event::Retry, t);
        let counting = m.generation;
        m.step(Event::Cancel, t + ms(500));
        for timer in [Timer::Tick, Timer::Settle, Timer::Fetch, Timer::Capture] {
            let stale = Event::Fired {
                generation: counting,
                timer,
            };
            assert_eq!(m.step(stale, t + ms(1000)), []);
            assert_eq!(m.phase, Phase::Idle);
        }
        // A new arm: the old arm's timers do not reach it either.
        m.step(Event::Interactive, t + ms(2000));
        let fetching = m.generation;
        for generation in 0..fetching {
            for timer in [Timer::Fetch, Timer::Tick, Timer::Settle, Timer::Capture] {
                let stale = Event::Fired { generation, timer };
                assert_eq!(m.step(stale, t + ms(2100)), []);
            }
        }
        assert_eq!((m.phase, m.generation), (Phase::Fetching, fetching));
        // A timer of the right generation but another kind is ignored too.
        assert_eq!(fire(&mut m, Timer::Settle, t + ms(2100)), []);
        assert_eq!(fire(&mut m, Timer::Tick, t + ms(2100)), []);
        assert_eq!(m.phase, Phase::Fetching);
    }

    #[test]
    fn answers_for_an_arm_since_cancelled_or_retried_are_ignored() {
        let t = Instant::now();
        let mut m = Machine::new(t);
        m.step(Event::Interactive, t);
        let first = m.generation;
        m.step(Event::Cancel, t);
        m.step(Event::Interactive, t + ms(10));
        let old = Event::Windows {
            generation: first,
            layout: layout("DP-1", &["old"]),
        };
        assert_eq!(m.step(old, t + ms(11)), []);
        assert_eq!(m.phase, Phase::Fetching);
        assert!(m.rects.is_empty());
        windows(&mut m, layout("eDP-1", &["new"]), t + ms(12));
        let capturing = m.generation;
        captured(&mut m, &["eDP-1"], t + ms(20));
        // Retried: frames of the capture before it are not shown again.
        m.step(Event::Retry, t + ms(30));
        let effects = m.step(
            Event::Captured {
                generation: capturing,
                outputs: outputs(&["eDP-1"]),
            },
            t + ms(40),
        );
        assert_eq!(effects, []);
        assert_eq!(m.phase, Phase::Settling);
    }

    #[test]
    fn the_delay_is_kept_to_0_to_10() {
        let t = Instant::now();
        let mut m = Machine::new(t);
        for (asked, kept) in [(-3, 0), (0, 0), (4, 4), (10, 10), (11, 10), (i32::MAX, 10)] {
            m.step(Event::Delay(asked), t);
            assert_eq!(m.delay(), kept, "{asked}");
        }
        m.step(Event::Delay(i32::MIN), t);
        assert_eq!(m.delay(), 0);
        // Set in any phase, it waits for the next retry.
        m.step(Event::Interactive, t);
        m.step(Event::Delay(5), t);
        assert_eq!((m.delay(), m.phase), (5, Phase::Fetching));
    }

    #[test]
    fn a_picked_window_ends_picking_however_often_it_is_heard() {
        let t = Instant::now();
        let mut m = Machine::new(t);
        shown(&mut m, t);
        m.step(Event::TogglePick, t);
        assert!(m.pick());
        let generation = m.generation;
        // Two picks before the first was heard: still off, not on again.
        for _ in 0..2 {
            assert_eq!(m.step(Event::Picked, t), []);
            assert!(!m.pick());
        }
        assert_eq!((m.phase, m.generation), (Phase::Shown, generation));
        m.step(Event::TogglePick, t);
        assert!(m.pick());
        // Heard when idle: nothing to end.
        m.step(Event::Cancel, t);
        assert_eq!(m.step(Event::Picked, t), []);
        assert!(!m.pick());
    }

    #[test]
    fn pick_mode_is_for_shown_frames_only() {
        let t = Instant::now();
        let mut m = Machine::new(t);
        m.step(Event::TogglePick, t);
        assert!(!m.pick);
        m.step(Event::Interactive, t);
        m.step(Event::TogglePick, t);
        assert!(!m.pick);
        windows(&mut m, layout("eDP-1", &[]), t);
        captured(&mut m, &["eDP-1"], t);
        m.step(Event::TogglePick, t);
        assert!(m.pick);
        assert_eq!(m.describe(), "shown on eDP-1, picking a window");
        m.step(Event::TogglePick, t);
        assert!(!m.pick);
    }

    #[test]
    fn the_pointer_moves_the_active_output_while_the_frames_are_up() {
        let t = Instant::now();
        let mut m = Machine::new(t);
        // Not before the frames are up: the window list says where it is.
        m.step(Event::Interactive, t);
        assert_eq!(m.step(Event::Pointer("DP-1".into()), t), []);
        assert_eq!(m.active, None);
        windows(&mut m, layout("eDP-1", &[]), t);
        captured(&mut m, &["DP-1", "eDP-1"], t);
        let (generation, since) = (m.generation, m.since());
        let keyboard = |name: &str| {
            vec![Effect::Keyboard {
                output: name.into(),
            }]
        };
        // The keyboard goes with it.
        assert_eq!(
            m.step(Event::Pointer("DP-1".into()), t + ms(5)),
            keyboard("DP-1")
        );
        assert_eq!(m.active.as_deref(), Some("DP-1"));
        // Not a change of phase.
        assert_eq!((m.generation, m.since()), (generation, since));
        // Back onto the output that has it: nothing to hand over.
        assert_eq!(m.step(Event::Pointer("DP-1".into()), t + ms(6)), []);
        // An output with no frame up is not one to be active.
        assert_eq!(m.step(Event::Pointer("HDMI-A-3".into()), t + ms(6)), []);
        assert_eq!(m.active.as_deref(), Some("DP-1"));
        assert_eq!(
            m.step(Event::Pointer("eDP-1".into()), t + ms(7)),
            keyboard("eDP-1")
        );
        assert_eq!(m.active.as_deref(), Some("eDP-1"));
        m.step(Event::Cancel, t + ms(8));
        assert_eq!(m.step(Event::Pointer("DP-1".into()), t + ms(9)), []);
        assert_eq!(m.active, None);
    }

    #[test]
    fn enter_saves_the_active_output_then_closes() {
        let t = Instant::now();
        let mut m = Machine::new(t);
        shown(&mut m, t);
        m.step(Event::TogglePick, t);
        m.step(Event::Pointer("DP-1".into()), t);
        let at = t + ms(40);
        let effects = m.step(Event::Save, at);
        assert_eq!(
            effects,
            [Effect::Save {
                output: "DP-1".into(),
                to: To::File,
                generation: m.generation
            }]
        );
        // The frames stay up while it is written.
        assert_eq!((m.phase, m.since()), (Phase::Saving(To::File), at));
        assert_eq!(m.describe(), "saving DP-1, shown on DP-1, eDP-1");
        assert!(m.pick && m.overlays.len() == 2);
        // Enter again, the toolbar, or Print: nothing, while it is.
        for event in [
            Event::Save,
            Event::Copy,
            Event::Retry,
            Event::TogglePick,
            Event::Interactive,
        ] {
            assert_eq!(m.step(event, at), []);
        }
        assert_eq!(m.phase, Phase::Saving(To::File));
        // Saved: closed.
        let done = t + ms(300);
        assert_eq!(saved(&mut m, None, done), [Effect::CloseOverlays]);
        assert_eq!((m.phase, m.since()), (Phase::Idle, done));
        assert!(!m.pick && m.active.is_none() && m.overlays.is_empty());
        // Idle, or not shown yet: Enter and copy do nothing.
        assert_eq!(m.step(Event::Save, at), []);
        m.step(Event::Interactive, at);
        assert_eq!(m.step(Event::Save, at), []);
        assert_eq!(m.step(Event::Copy, at), []);
        assert_eq!(m.phase, Phase::Fetching);
        // With the pointer never moved, the output it was on at the arm.
        let mut m = Machine::new(t);
        shown(&mut m, t);
        let effects = m.step(Event::Save, at);
        assert!(
            matches!(&effects[..], [Effect::Save { output, .. }] if output == "eDP-1"),
            "{effects:?}"
        );
    }

    #[test]
    fn copy_puts_the_active_outputs_shot_on_the_clipboard_then_closes() {
        let t = Instant::now();
        let mut m = Machine::new(t);
        shown(&mut m, t);
        m.step(Event::Pointer("DP-1".into()), t);
        assert_eq!(
            m.step(Event::Copy, t),
            [Effect::Save {
                output: "DP-1".into(),
                to: To::Clipboard,
                generation: m.generation
            }]
        );
        assert_eq!(m.describe(), "copying DP-1, shown on DP-1, eDP-1");
        assert_eq!(saved(&mut m, None, t + ms(90)), [Effect::CloseOverlays]);
        assert_eq!(m.phase, Phase::Idle);
    }

    /// The shot is not lost: the frames stay as they were, and the overlay
    /// the keyboard is on says why, for her to try again, copy, or let go.
    #[test]
    fn a_failed_save_or_copy_puts_the_frames_back_and_says_why() {
        for (event, to) in [(Event::Save, To::File), (Event::Copy, To::Clipboard)] {
            let t = Instant::now();
            let mut m = Machine::new(t);
            shown(&mut m, t);
            m.step(Event::TogglePick, t);
            m.step(event, t);
            // The pointer went to the other output meanwhile, and the
            // keyboard with it: that is where it is said.
            assert_eq!(
                m.step(Event::Pointer("DP-1".into()), t),
                [Effect::Keyboard {
                    output: "DP-1".into()
                }]
            );
            let at = t + ms(500);
            assert_eq!(
                saved(&mut m, Some("No space left on device"), at),
                [Effect::Failed {
                    output: "DP-1".into(),
                    to,
                    why: "No space left on device".into()
                }]
            );
            assert_eq!((m.phase, m.since()), (Phase::Shown, at));
            // Nothing closed or forgotten: the frames, their windows,
            // picking.
            assert_eq!(m.overlays, outputs(&["DP-1", "eDP-1"]));
            assert!(m.pick && !m.rects.is_empty());
            // Enter tries again; Esc lets go.
            let effects = m.step(Event::Save, at);
            assert!(matches!(effects[..], [Effect::Save { .. }]), "{effects:?}");
            assert_eq!(m.step(Event::Cancel, at), [Effect::CloseOverlays]);
        }
    }

    #[test]
    fn an_answer_to_a_save_since_cancelled_is_ignored() {
        let t = Instant::now();
        let mut m = Machine::new(t);
        shown(&mut m, t);
        m.step(Event::Save, t);
        let asked = m.generation;
        m.step(Event::Cancel, t);
        for failure in [None, Some("late".to_string())] {
            let late = Event::Saved {
                generation: asked,
                failure,
            };
            assert_eq!(m.step(late, t + ms(10)), []);
            assert_eq!(m.phase, Phase::Idle);
        }
        // Nor does it end the next arm's save.
        shown(&mut m, t + ms(400));
        m.step(Event::Copy, t + ms(410));
        let late = Event::Saved {
            generation: asked,
            failure: None,
        };
        assert_eq!(m.step(late, t + ms(420)), []);
        assert_eq!(m.phase, Phase::Saving(To::Clipboard));
    }

    /// As in the QML: the frames stay up until Esc, Enter or a cancel,
    /// however long that is. No timer closes them.
    #[test]
    fn the_frames_stay_up_until_told() {
        let t = Instant::now();
        let mut m = Machine::new(t);
        shown(&mut m, t);
        m.step(Event::Pointer("DP-1".into()), t + ms(10));
        let later = t + Duration::from_secs(3600);
        for timer in [Timer::Fetch, Timer::Capture, Timer::Tick, Timer::Settle] {
            assert_eq!(fire(&mut m, timer, later), [], "{timer:?}");
        }
        assert_eq!(m.phase, Phase::Shown);
        assert_eq!(m.active.as_deref(), Some("DP-1"));
        assert_eq!(
            m.step(Event::Cancel, later),
            [Effect::CloseOverlays],
            "Esc still closes them"
        );
    }

    #[test]
    fn print_right_after_a_cancel_waits_the_settle_out() {
        for (answered, settled) in [(249, false), (250, true), (251, true)] {
            let t = Instant::now();
            let mut m = Machine::new(t);
            shown(&mut m, t);
            let cancelled = t + ms(1000);
            m.step(Event::Cancel, cancelled);
            // Print at once: asking for the window list captures nothing.
            let effects = m.step(Event::Interactive, cancelled + ms(1));
            assert!(!captures(&effects));
            let at = cancelled + ms(answered);
            let effects = windows(&mut m, layout("eDP-1", &[]), at);
            if settled {
                assert!(captures(&effects), "{answered}: {effects:?}");
                continue;
            }
            assert_eq!(
                effects,
                [Effect::Start {
                    timer: Timer::Settle,
                    after: ms(1),
                    generation: m.generation
                }],
                "{answered}"
            );
            assert_eq!(m.phase, Phase::Settling);
            // Then the window list again, and the capture.
            let effects = fire(&mut m, Timer::Settle, cancelled + ms(250));
            assert!(matches!(effects[0], Effect::FetchWindows { .. }));
            let effects = windows(&mut m, layout("eDP-1", &[]), cancelled + ms(252));
            assert!(captures(&effects), "{effects:?}");
        }
    }

    #[test]
    fn a_settle_timer_that_fires_early_captures_nothing_early() {
        // set_timeout never fires early, but the rule does not lean on it.
        for (after, settled) in [(249, false), (250, true)] {
            let t = Instant::now();
            let mut m = Machine::new(t);
            shown(&mut m, t);
            m.step(Event::Retry, t);
            fire(&mut m, Timer::Settle, t + ms(10));
            let effects = windows(&mut m, layout("eDP-1", &[]), t + ms(after));
            assert_eq!(captures(&effects), settled, "{after}: {effects:?}");
        }
    }

    /// A small deterministic random walk (an LCG; no crate for it).
    struct Walk(u64);

    impl Walk {
        fn next(&mut self, n: u64) -> u64 {
            self.0 = self
                .0
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            (self.0 >> 33) % n
        }
    }

    /// What the daemon would have on screen, from the effects alone.
    #[derive(Default)]
    struct Screen {
        overlays: bool,
        /// The outputs whose overlays are open.
        up: Vec<String>,
        /// The one with the keyboard.
        keyboard: Option<String>,
        countdown: bool,
        closed_at: Option<Instant>,
        captures: usize,
        saves: usize,
        failures: usize,
    }

    impl Screen {
        fn apply(&mut self, effects: &[Effect], now: Instant) {
            for effect in effects {
                match effect {
                    Effect::Capture { .. } => {
                        assert!(!self.overlays, "a capture with the overlays open");
                        assert!(!self.countdown, "a capture with the countdown open");
                        if let Some(closed) = self.closed_at {
                            assert!(
                                now.duration_since(closed) >= SETTLE,
                                "a capture {:?} after a layer closed",
                                now.duration_since(closed)
                            );
                        }
                        self.captures += 1;
                    }
                    Effect::OpenOverlays { outputs, keyboard } => {
                        assert!(!self.overlays && !self.countdown);
                        assert!(!outputs.is_empty() && outputs.contains(keyboard));
                        self.overlays = true;
                        self.up = outputs.clone();
                        self.keyboard = Some(keyboard.clone());
                    }
                    // Only to another overlay that is open.
                    Effect::Keyboard { output } => {
                        assert!(self.up.contains(output), "the keyboard to {output}");
                        assert_ne!(self.keyboard.as_ref(), Some(output));
                        self.keyboard = Some(output.clone());
                    }
                    Effect::CloseOverlays => {
                        assert!(self.overlays, "closing overlays that are not open");
                        self.overlays = false;
                        self.up.clear();
                        self.keyboard = None;
                        self.closed_at = Some(now);
                    }
                    // Only a frame that is up, the one with the keyboard.
                    Effect::Save { output, .. } => {
                        assert!(
                            self.up.contains(output),
                            "saving {output} with {:?} up",
                            self.up
                        );
                        assert_eq!(self.keyboard.as_ref(), Some(output));
                        self.saves += 1;
                    }
                    // Said where it is seen, and Esc reaches.
                    Effect::Failed { output, .. } => {
                        assert!(self.up.contains(output), "failed on {output}");
                        assert_eq!(self.keyboard.as_ref(), Some(output));
                        self.failures += 1;
                    }
                    Effect::Countdown { left, of, .. } => {
                        assert!(!self.overlays);
                        assert!(*left >= 1 && left <= of && *of <= MAX_DELAY);
                        self.countdown = true;
                    }
                    Effect::CloseCountdown => {
                        assert!(self.countdown, "closing a countdown that is not open");
                        self.countdown = false;
                        self.closed_at = Some(now);
                    }
                    _ => {}
                }
            }
        }
    }

    /// What the effects ask to come back: answers and timers. Ones for a
    /// phase that is over stay in the pile, to come back stale.
    fn answers(effects: &[Effect], walk: &mut Walk, pile: &mut Vec<Event>) {
        for effect in effects {
            let names: &[&str] = [&[][..], &["eDP-1"], &["DP-1", "eDP-1"]][walk.next(3) as usize];
            pile.push(match *effect {
                Effect::FetchWindows { generation } => Event::Windows {
                    generation,
                    layout: [None, layout("eDP-1", &["kitty"])][walk.next(2) as usize].clone(),
                },
                Effect::Capture { generation } => Event::Captured {
                    generation,
                    outputs: outputs(names),
                },
                Effect::Save { generation, .. } => Event::Saved {
                    generation,
                    failure: [None, Some("No space left on device".into())][walk.next(2) as usize]
                        .clone(),
                },
                Effect::Start {
                    timer, generation, ..
                } => Event::Fired { generation, timer },
                _ => continue,
            });
        }
    }

    #[test]
    fn nothing_is_captured_with_a_layer_open_or_just_closed() {
        let t0 = Instant::now();
        let mut near = 0;
        let mut saves = 0;
        let mut failures = 0;
        let mut handovers = 0;
        let mut skipped = 0;
        for seed in 0..200 {
            let mut walk = Walk(seed);
            let mut now = t0;
            let mut m = Machine::new(now);
            let mut screen = Screen::default();
            let mut pile = Vec::new();
            for _ in 0..1000 {
                // Steps that add up to either side of the settle's edge.
                now += ms([0, 1, 3, 5, 120, 245, 249, 250, 1000][walk.next(9) as usize]);
                // Mostly what the machine waits for, in any order and at
                // any time, early or late; else her keys and the toolbar.
                let event = if !pile.is_empty() && walk.next(3) > 0 {
                    pile.swap_remove(walk.next(pile.len() as u64) as usize)
                } else {
                    match walk.next(11) {
                        0 | 1 => Event::Interactive,
                        2 => Event::Cancel,
                        3 => Event::Retry,
                        4 => Event::TogglePick,
                        5 => Event::Picked,
                        6 => Event::Save,
                        10 => Event::Copy,
                        7 | 8 => {
                            let name = ["DP-1", "eDP-1", "HDMI-A-3"][walk.next(3) as usize];
                            Event::Pointer(name.into())
                        }
                        _ => Event::Delay(walk.next(14) as i32 - 2),
                    }
                };
                let captures = screen.captures;
                let print = event == Event::Interactive;
                let effects = m.step(event, now);
                if print && effects.contains(&Effect::CloseCountdown) {
                    skipped += 1;
                }
                screen.apply(&effects, now);
                if screen.captures > captures
                    && screen.closed_at.is_some_and(|c| now - c < SETTLE * 2)
                {
                    near += 1;
                }
                answers(&effects, &mut walk, &mut pile);
                assert_eq!(screen.overlays, !m.overlays.is_empty());
                assert_eq!(screen.countdown, m.countdown);
                assert!(m.delay() <= MAX_DELAY);
                // While the frames are up, saving or not, the active output
                // is one of them, and has the keyboard: Esc always reaches
                // an overlay.
                if m.up() {
                    assert!(m.active.as_ref().is_some_and(|a| m.overlays.contains(a)));
                    assert_eq!(screen.keyboard, m.active);
                    handovers += effects
                        .iter()
                        .filter(|e| matches!(e, Effect::Keyboard { .. }))
                        .count();
                } else {
                    assert_eq!(screen.keyboard, None);
                }
                // Picking is only while the frames are up.
                assert!(!m.pick || m.up());
            }
            assert!(
                screen.captures > 20,
                "seed {seed}: {} captures",
                screen.captures
            );
            saves += screen.saves;
            failures += screen.failures;
        }
        assert!(saves > 200, "{saves} saves");
        assert!(failures > 100, "{failures} failed saves");
        assert!(skipped > 20, "{skipped} countdowns cut short by Print");
        assert!(handovers > 200, "{handovers} handovers of the keyboard");
        // Many captures came soon after a layer closed, at the edge's
        // side that is allowed.
        assert!(near > 100, "{near} captures within 500 ms of a close");
    }
}
