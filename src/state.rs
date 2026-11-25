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
    /// Before a retry, with this many seconds left.
    Counting(u8),
    /// Waiting for the screen to be drawn without MinkaShot's layers.
    Settling,
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
    /// Print, or `minkashot interactive`: arm, when idle.
    Interactive,
    /// Esc, or `minkashot cancel`: back to idle from anywhere.
    Cancel,
    /// The toolbar's retry: close the overlays, count down the delay and
    /// freeze again.
    #[cfg_attr(
        not(test),
        expect(dead_code, reason = "the overlay's toolbar sends it (slice 6)")
    )]
    Retry,
    /// The toolbar's window toggle.
    #[cfg_attr(
        not(test),
        expect(dead_code, reason = "the overlay's toolbar sends it (slice 5)")
    )]
    TogglePick,
    /// The toolbar's delay stepper: the seconds before a retry, kept to
    /// 0..=[`MAX_DELAY`].
    #[cfg_attr(
        not(test),
        expect(dead_code, reason = "the overlay's toolbar sends it (slice 6)")
    )]
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
    /// Show the countdown at `left` seconds on `output`, the one that had
    /// the keyboard, opening its layer if it is not open.
    Countdown {
        output: String,
        left: u8,
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
    pick: bool,
    /// The output with the keyboard: the one under the pointer at the arm.
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
            Phase::Counting(left) => format!("counting down to a retry, {left} s left"),
            Phase::Settling => "waiting for the screen to settle before a capture".into(),
        }
    }

    pub fn step(&mut self, event: Event, now: Instant) -> Vec<Effect> {
        let mut out = Vec::new();
        match event {
            // Print while armed, counting down or settling does nothing.
            Event::Interactive if self.phase == Phase::Idle => self.fetch(now, &mut out),
            Event::Cancel if self.phase != Phase::Idle => self.idle(now, &mut out),
            Event::Retry if self.phase == Phase::Shown => {
                self.close_layers(now, &mut out);
                self.pick = false;
                if self.delay > 0 {
                    self.count(self.delay, now, &mut out);
                } else {
                    self.settle(SETTLE, now, &mut out);
                }
            }
            Event::TogglePick if self.phase == Phase::Shown => self.pick = !self.pick,
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

    fn count(&mut self, left: u8, now: Instant, out: &mut Vec<Effect>) {
        self.countdown = true;
        self.enter(Phase::Counting(left), now);
        out.push(Effect::Countdown {
            // Named since the frames were shown, which a countdown follows.
            output: self.active.clone().unwrap_or_default(),
            left,
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
        assert_eq!(
            captured(&mut m, &["DP-1", "eDP-1"], t + ms(90)),
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
    fn print_while_not_idle_does_nothing() {
        let t = Instant::now();
        let mut m = Machine::new(t);
        m.step(Event::Delay(2), t);
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
        m.step(Event::Retry, t);
        check(&mut m);
        fire(&mut m, Timer::Tick, t);
        fire(&mut m, Timer::Tick, t);
        check(&mut m);
        assert_eq!(
            seen,
            [
                Phase::Fetching,
                Phase::Fetching,
                Phase::Capturing,
                Phase::Shown,
                Phase::Counting(2),
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
        let cases: [(Setup, &[Effect]); 5] = [
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
        };
        let effects = m.step(Event::Retry, t);
        assert_eq!(effects, [Effect::CloseOverlays, countdown(3), tick(&m)]);
        assert_eq!(m.describe(), "counting down to a retry, 3 s left");
        let effects = fire(&mut m, Timer::Tick, t + ms(1000));
        assert_eq!(effects, [countdown(2), tick(&m)]);
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
        assert_eq!(m.delay(), 3);
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
        countdown: bool,
        closed_at: Option<Instant>,
        captures: usize,
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
                    Effect::OpenOverlays { .. } => {
                        assert!(!self.overlays && !self.countdown);
                        self.overlays = true;
                    }
                    Effect::CloseOverlays => {
                        assert!(self.overlays, "closing overlays that are not open");
                        self.overlays = false;
                        self.closed_at = Some(now);
                    }
                    Effect::Countdown { .. } => {
                        assert!(!self.overlays);
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
                    match walk.next(6) {
                        0 | 1 => Event::Interactive,
                        2 => Event::Cancel,
                        3 => Event::Retry,
                        4 => Event::TogglePick,
                        _ => Event::Delay(walk.next(14) as i32 - 2),
                    }
                };
                let captures = screen.captures;
                let effects = m.step(event, now);
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
            }
            assert!(
                screen.captures > 20,
                "seed {seed}: {} captures",
                screen.captures
            );
        }
        // Many captures came soon after a layer closed, at the edge's
        // side that is allowed.
        assert!(near > 100, "{near} captures within 500 ms of a close");
    }
}
