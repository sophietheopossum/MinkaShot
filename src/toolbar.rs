//! The toolbar over the frozen frames, as the QML ControlPanel was: the
//! title, the delay before a retry, picking a window, retry, copy, and what
//! the keys do. It is md3e's floating toolbar in its compact size, with
//! the old panel's outline. Every overlay has one, showing the machine's
//! pick and delay as the daemon keeps them, so they are the same on each.
//!
//! A press on it is the toolbar's: none starts a region, a press on a
//! button that is off included. A press moves the focus to the button, and
//! the overlay takes it back at once (`overlay::view`), so Esc and Enter
//! are the overlay's after a click as before. A stepper button turned off
//! while it has the focus gives it back to the overlay itself.

use anti_webui::core::animation::request_frame;
use anti_webui::core::event::PointerEvent;
use anti_webui::core::prelude::{Binding, Div, NodeRef, div, node_ref, text};
use anti_webui::core::style::{CursorIcon, Styled};
use anti_webui_md3e::components::{
    ButtonSize, ButtonStyle, IconButtonStyle, ToolbarStyle, floating_toolbar, styled_icon_button,
    styled_toggle_button, tooltip,
};
use anti_webui_md3e::icons;
use std::rc::Rc;

use crate::overlay::{Shared, Told};
use crate::state::{Event, MAX_DELAY};
use crate::theme::{
    FONT_SIZE, LINE, MONO_FAMILY, RED, TEXT, TEXT_FAINT, color, mono_text, ui_text,
};

/// How far the toolbar is from the top of the output, in logical pixels.
pub const TOP: f32 = 28.0;
/// Room beside the words, so they keep off the pill's round ends, and
/// before DELAY.
const ROOM: f32 = 8.0;
const BEFORE_DELAY: f32 = 4.0;
/// The QML's letter spacing of the title and of DELAY.
const TITLE_SPACING: f32 = 2.0;
const LABEL_SPACING: f32 = 1.0;
/// As wide for 10 s as for 0 s, so nothing moves as it changes.
const SECONDS: f32 = 24.0;
/// The QML panel's outline, so it shows on a dark screen too.
const OUTLINE: f32 = 1.0;

/// The toolbar's buttons, where the tests press them.
#[derive(Clone, Copy)]
pub struct Controls {
    pub less: NodeRef,
    pub more: NodeRef,
    pub pick: NodeRef,
    pub retry: NodeRef,
    pub copy: NodeRef,
}

impl Controls {
    pub fn new() -> Self {
        Controls {
            less: node_ref(),
            more: node_ref(),
            pick: node_ref(),
            retry: node_ref(),
            copy: node_ref(),
        }
    }
}

/// The toolbar of the overlay whose root is `root`, telling the daemon
/// what is pressed.
pub fn toolbar(shared: Shared, root: NodeRef, tell: Rc<dyn Fn(Told)>, controls: Controls) -> Div {
    let Shared { pick, delay, .. } = shared;
    // What a button tells, made from the delay it shows.
    let say = move |event: fn(u8) -> Event| {
        let tell = tell.clone();
        move || tell(Told::Event(event(delay.get_untracked())))
    };
    let click = |event: fn(u8) -> Event| {
        let said = say(event);
        move |_: &PointerEvent| said()
    };
    let small = || IconButtonStyle::standard().size(ButtonSize::ExtraSmall);
    // A stepper button turned off at its end while it has the focus leaves
    // nothing focused, and the overlay's trap never hears of it. That is
    // when a click's press and release come together: the machine's answer
    // turns the button off before the overlay has taken the focus back. The
    // button gives it back, at a frame it asks for.
    let give_back = move |on: bool| {
        if !on {
            request_frame(move |_| root.focus());
        }
    };

    let title = div().px(ROOM).child(
        mono_text("MINKASHOT", FONT_SIZE - 1.0, RED)
            .bold()
            .letter_spacing(TITLE_SPACING),
    );
    let label = div()
        .pl(BEFORE_DELAY)
        .child(mono_text("DELAY", FONT_SIZE - 3.0, TEXT_FAINT).letter_spacing(LABEL_SPACING));
    let less = styled_icon_button(
        icons::REMOVE,
        small().disabled_when(move || delay.get() == 0),
    )
    .node_ref(controls.less)
    .on_focus_change(give_back)
    .on_click(click(|d| Event::Delay(d as i32 - 1)));
    let seconds = div()
        .min_w(SECONDS)
        .flex()
        .justify_center()
        .dyn_style(move |s| s.text_color(color(if delay.get() > 0 { TEXT } else { TEXT_FAINT })))
        .child(
            text(move || format!("{}s", delay.get()))
                .font_family(MONO_FAMILY)
                .text_size(FONT_SIZE - 1.0),
        );
    let more = styled_icon_button(
        icons::ADD,
        small().disabled_when(move || delay.get() >= MAX_DELAY),
    )
    .node_ref(controls.more)
    .on_focus_change(give_back)
    .on_click(click(|d| Event::Delay(d as i32 + 1)));
    // On while picking, as the machine says; a click asks it to change.
    let picking = {
        let toggle = say(|_| Event::TogglePick);
        Binding::new(move || pick.get(), move |_| toggle())
    };
    let window = styled_toggle_button(
        "Window",
        picking,
        ButtonStyle::outlined()
            .size(ButtonSize::ExtraSmall)
            .icon(icons::SELECT_WINDOW),
    )
    .node_ref(controls.pick);
    let retry = styled_icon_button(icons::REFRESH, small())
        .node_ref(controls.retry)
        .on_click(click(|_| Event::Retry));
    let copy = tooltip(
        styled_icon_button(icons::CONTENT_COPY, small())
            .node_ref(controls.copy)
            .on_click(click(|_| Event::Copy)),
        "Copy to the clipboard",
    );
    let hint = div().px(ROOM).child(ui_text(
        "enter save · esc cancel · drag region",
        FONT_SIZE - 3.0,
        TEXT_FAINT,
    ));

    floating_toolbar(
        vec![title, label, less, seconds, more, window, retry, copy, hint],
        ToolbarStyle::new(),
    )
    .border(OUTLINE)
    .border_color(color(LINE))
    // The pointer, not the crosshair, to aim at its buttons.
    .cursor(CursorIcon::Default)
    .pointer_events_auto()
    .on_pointer_down(|ev| ev.stop_propagation())
    .on_pointer_up(|ev| ev.stop_propagation())
}
