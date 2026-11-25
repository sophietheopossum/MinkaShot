//! Picking a window for `minkashot win`: by the role a program claimed for
//! it (`minkamon.disk`), its title, its app id, a piece of either, or the
//! focused one.
//!
//! The windows are the toolkit's list (`anti_webui::shell::toplevels`),
//! which is what its capture takes. Roles are only in the compositor's view
//! (`workspaces.get`), which gives each window's title and app id, so a role
//! is found there and then matched to the list by those.
//!
//! The same view gives the daemon the windows to pick from in the overlay
//! ([`rects`]) and the output under the pointer ([`current_monitor`]).

use anti_webui::shell::Toplevel;
use serde_json::Value;
use std::time::Duration;

use crate::cli::Failure;
use crate::geometry::Rect;

/// How long the compositor may take to say which window has a role.
pub const ROLE_TIMEOUT: Duration = Duration::from_secs(1);

/// How long the window list may take to be complete, after the toolkit's own
/// wait for it (half a second).
pub const LIST_TIMEOUT: Duration = Duration::from_secs(1);

/// What a window is known by.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Seen {
    pub app_id: String,
    pub title: String,
    pub focused: bool,
    pub minimized: bool,
}

impl From<&Toplevel> for Seen {
    fn from(t: &Toplevel) -> Self {
        Seen {
            app_id: t.app_id.clone(),
            title: t.title.clone(),
            focused: t.activated,
            minimized: t.minimized,
        }
    }
}

/// A window the compositor's view gives a role: its title, and its app id
/// when the view knows it.
#[derive(Clone, Debug, PartialEq)]
pub struct Named {
    pub app_id: Option<String>,
    pub title: String,
}

/// What ShojiWM's window rects have around the window itself: the 14 px
/// ring that edge drags start in. A pick wants the visible border.
pub const CHROME_INSET: f64 = 14.0;

/// A window the overlay can pick, as the compositor placed it.
#[derive(Clone, Debug, PartialEq)]
pub struct WindowRect {
    pub title: String,
    /// The output it is on.
    pub monitor: String,
    /// In global logical pixels, [`CHROME_INSET`] taken off each side.
    pub rect: Rect,
    /// When it last had the keyboard, in Unix milliseconds; 0 for never.
    pub last_focused_at: f64,
}

fn list<'a>(v: &'a Value, key: &str) -> &'a [Value] {
    v.get(key)
        .and_then(Value::as_array)
        .map_or(&[], Vec::as_slice)
}

fn text(v: &Value, key: &str) -> Option<String> {
    v.get(key).and_then(Value::as_str).map(str::to_owned)
}

/// The windows a `workspaces.get` view shows, each with its output's name:
/// those on an active workspace and not minimised, as the QML MinkaShot
/// took them.
fn shown(view: &Value) -> Vec<(&str, &Value)> {
    let mut shown = Vec::new();
    for monitor in list(view, "monitors") {
        let name = monitor.get("name").and_then(Value::as_str).unwrap_or("");
        for workspace in list(monitor, "workspaces") {
            if workspace.get("active").and_then(Value::as_bool) != Some(true) {
                continue;
            }
            for w in list(workspace, "windows") {
                if w.get("minimized").and_then(Value::as_bool) != Some(true) {
                    shown.push((name, w));
                }
            }
        }
    }
    shown
}

/// The windows with `role` in a `workspaces.get` view (see [`shown`]).
pub fn with_role(view: &Value, role: &str) -> Vec<Named> {
    shown(view)
        .into_iter()
        .filter(|(_, w)| text(w, "role").as_deref() == Some(role))
        .filter_map(|(_, w)| {
            Some(Named {
                app_id: text(w, "appId"),
                title: text(w, "title")?,
            })
        })
        .collect()
}

/// The windows of a `workspaces.get` view to pick from (see [`shown`]), top
/// first. The compositor does not say how they stack, so the one that had
/// the keyboard last counts as the top, as in the QML and MinkaMon.
pub fn rects(view: &Value) -> Vec<WindowRect> {
    let mut rects: Vec<WindowRect> = shown(view)
        .into_iter()
        .filter_map(|(monitor, w)| {
            let rect = w.get("rect")?;
            let n = |key: &str| rect.get(key).and_then(Value::as_f64);
            Some(WindowRect {
                title: text(w, "title").unwrap_or_default(),
                monitor: monitor.to_owned(),
                rect: Rect {
                    x: n("x")? + CHROME_INSET,
                    y: n("y")? + CHROME_INSET,
                    width: n("width")? - 2.0 * CHROME_INSET,
                    height: n("height")? - 2.0 * CHROME_INSET,
                },
                last_focused_at: w
                    .get("lastFocusedAt")
                    .and_then(Value::as_f64)
                    .unwrap_or(0.0),
            })
        })
        .collect();
    // Stable, so windows focused at the same moment keep the view's order.
    rects.sort_by(|a, b| b.last_focused_at.total_cmp(&a.last_focused_at));
    rects
}

/// The output under the pointer in a `workspaces.get` view.
pub fn current_monitor(view: &Value) -> Option<String> {
    view.get("currentMonitor")
        .and_then(Value::as_str)
        .filter(|m| !m.is_empty())
        .map(str::to_owned)
}

/// The windows with `role`, from the compositor's IPC.
pub fn roles(role: &str) -> Result<Vec<Named>, String> {
    let view = minka_ipc::request_blocking("workspaces.get", serde_json::json!({}), ROLE_TIMEOUT)
        .map_err(|e| format!("the compositor's IPC: {e}"))?;
    Ok(with_role(&view, role))
}

/// Which of `windows` is meant: for none (or `""`) the focused one, else
/// the first of these that matches any, in order:
///
/// 1. the role, which `roles` looks up;
/// 2. the whole title;
/// 3. the whole app id;
/// 4. a piece of the title or the app id, in any case.
///
/// Where several match, minimised windows count only if all are, and then
/// the focused one is meant; if none of them is, nothing is, and the
/// failure lists them.
pub fn pick(
    asked: Option<&str>,
    windows: &[Seen],
    roles: impl FnOnce(&str) -> Result<Vec<Named>, String>,
) -> Result<usize, Failure> {
    let Some(asked) = asked.filter(|a| !a.is_empty()) else {
        return windows.iter().position(|w| w.focused).ok_or_else(|| {
            Failure::NoWindow(
                "no window has the keyboard: name one (minkashot list shows them)".into(),
            )
        });
    };
    let all = || (0..windows.len()).collect::<Vec<_>>();
    let matching = |is: &dyn Fn(&Seen) -> bool| -> Vec<usize> {
        all().into_iter().filter(|&i| is(&windows[i])).collect()
    };

    match roles(asked) {
        Ok(named) if !named.is_empty() => {
            let found = matching(&|w| {
                named
                    .iter()
                    .any(|n| n.title == w.title && n.app_id.as_ref().is_none_or(|a| *a == w.app_id))
            });
            if found.is_empty() {
                return Err(Failure::NoWindow(format!(
                    "the window with the role {asked:?} ({:?}) is not in the window list",
                    named[0].title
                )));
            }
            return settle(asked, windows, found);
        }
        Ok(_) => {}
        Err(why) => minka_note::note!("minkashot: cannot look up roles ({why}); matching titles"),
    }

    let lower = asked.to_lowercase();
    let tiers: [&dyn Fn(&Seen) -> bool; 3] =
        [&|w| w.title == asked, &|w| w.app_id == asked, &|w| {
            w.title.to_lowercase().contains(&lower) || w.app_id.to_lowercase().contains(&lower)
        }];
    for tier in tiers {
        let found = matching(tier);
        if !found.is_empty() {
            return settle(asked, windows, found);
        }
    }
    Err(Failure::NoWindow(format!(
        "no window matches {asked:?} (minkashot list shows them)"
    )))
}

/// One of several windows that match: see [`pick`].
fn settle(asked: &str, windows: &[Seen], mut found: Vec<usize>) -> Result<usize, Failure> {
    if found.iter().any(|&i| !windows[i].minimized) {
        found.retain(|&i| !windows[i].minimized);
    }
    if let [one] = found[..] {
        return Ok(one);
    }
    if let Some(&focused) = found.iter().find(|&&i| windows[i].focused) {
        return Ok(focused);
    }
    let names: Vec<String> = found.iter().map(|&i| describe(&windows[i])).collect();
    Err(Failure::NoWindow(format!(
        "{} windows match {asked:?}, and none has the keyboard: {}",
        found.len(),
        names.join("; ")
    )))
}

/// `app id: title`, or the title alone when there is no app id.
pub fn describe(w: &Seen) -> String {
    if w.app_id.is_empty() {
        format!("{:?}", w.title)
    } else {
        format!("{}: {:?}", w.app_id, w.title)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn seen(app_id: &str, title: &str) -> Seen {
        Seen {
            app_id: app_id.into(),
            title: title.into(),
            ..Default::default()
        }
    }

    fn no_roles(_: &str) -> Result<Vec<Named>, String> {
        Ok(Vec::new())
    }

    fn windows() -> Vec<Seen> {
        vec![
            seen("kitty", "fish /home"),
            seen("floorp", "Kitty Hawk - Floorp"),
            seen("minkamon", "MinkaMon"),
            seen("minkamon", "MinkaMon Disk"),
        ]
    }

    #[test]
    fn no_name_is_the_focused_window() {
        let mut list = windows();
        list[1].focused = true;
        for asked in [None, Some("")] {
            assert_eq!(pick(asked, &list, |_| panic!("not asked")), Ok(1));
        }
        list[1].focused = false;
        assert!(matches!(
            pick(None, &list, no_roles),
            Err(Failure::NoWindow(_))
        ));
    }

    #[test]
    fn a_whole_title_beats_an_app_id_and_a_piece() {
        assert_eq!(pick(Some("MinkaMon"), &windows(), no_roles), Ok(2));
        assert_eq!(pick(Some("kitty"), &windows(), no_roles), Ok(0));
        // A piece, in any case: only one title or app id has "hawk".
        assert_eq!(pick(Some("hawk"), &windows(), no_roles), Ok(1));
    }

    #[test]
    fn a_role_comes_first() {
        let role = |r: &str| {
            assert_eq!(r, "minkamon.disk");
            Ok(vec![Named {
                app_id: Some("minkamon".into()),
                title: "MinkaMon Disk".into(),
            }])
        };
        assert_eq!(pick(Some("minkamon.disk"), &windows(), role), Ok(3));
        // A role whose window the list lacks is no window, not a title match.
        let gone = |_: &str| {
            Ok(vec![Named {
                app_id: None,
                title: "Gone".into(),
            }])
        };
        assert!(matches!(
            pick(Some("minkamon.disk"), &windows(), gone),
            Err(Failure::NoWindow(_))
        ));
    }

    #[test]
    fn without_roles_titles_still_match() {
        let down = |_: &str| Err("no socket".to_string());
        assert_eq!(pick(Some("MinkaMon Disk"), &windows(), down), Ok(3));
    }

    #[test]
    fn several_matches_need_the_focus() {
        let mut list = windows();
        // "minkamon" is the app id of two windows.
        match pick(Some("minkamon"), &list, no_roles) {
            Err(f @ Failure::NoWindow(_)) => {
                assert_eq!(f.code(), 3);
                assert!(f.to_string().contains("2 windows match"), "{f}");
            }
            other => panic!("{other:?}"),
        }
        list[3].focused = true;
        assert_eq!(pick(Some("minkamon"), &list, no_roles), Ok(3));
    }

    #[test]
    fn a_minimised_window_counts_only_alone() {
        let mut list = windows();
        list[2].minimized = true;
        assert_eq!(pick(Some("minkamon"), &list, no_roles), Ok(3));
        list[3].minimized = true;
        assert!(pick(Some("minkamon"), &list, no_roles).is_err());
        assert_eq!(pick(Some("MinkaMon"), &list, no_roles), Ok(2));
    }

    #[test]
    fn nothing_matching_is_exit_status_3() {
        match pick(Some("nope"), &windows(), no_roles) {
            Err(f @ Failure::NoWindow(_)) => assert_eq!(f.code(), 3),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn roles_are_on_active_workspaces_and_not_minimised() {
        let view = json!({
            "currentMonitor": "eDP-1",
            "monitors": [{
                "name": "eDP-1",
                "workspaces": [
                    {"active": true, "windows": [
                        {"appId": "minkamon", "title": "MinkaMon Disk", "role": "minkamon.disk", "minimized": false},
                        {"appId": "minkamon", "title": "MinkaMon CPU", "role": "minkamon.cpu", "minimized": false},
                        {"appId": null, "title": "Hidden Disk", "role": "minkamon.disk", "minimized": true},
                    ]},
                    {"active": false, "windows": [
                        {"appId": "minkamon", "title": "Elsewhere", "role": "minkamon.disk", "minimized": false},
                    ]},
                ],
            }],
        });
        assert_eq!(
            with_role(&view, "minkamon.disk"),
            [Named {
                app_id: Some("minkamon".into()),
                title: "MinkaMon Disk".into()
            }]
        );
        assert_eq!(with_role(&view, "minkamon.gpu"), []);
        assert_eq!(with_role(&json!({}), "minkamon.disk"), []);
    }

    #[test]
    fn rects_are_shown_windows_inset_and_last_focused_first() {
        let window = |title: &str, x: f64, focused_at: f64| {
            json!({
                "title": title, "minimized": false, "lastFocusedAt": focused_at,
                "rect": {"x": x, "y": 100, "width": 800, "height": 600},
            })
        };
        let view = json!({
            "currentMonitor": "DP-1",
            "monitors": [
                {"name": "eDP-1", "workspaces": [
                    {"active": true, "windows": [
                        window("older", 0.0, 1000.0),
                        {"title": "hidden", "minimized": true, "lastFocusedAt": 9000,
                         "rect": {"x": 0, "y": 0, "width": 100, "height": 100}},
                        {"title": "no rect", "minimized": false, "lastFocusedAt": 9000},
                        window("newest", 50.0, 3000.0),
                    ]},
                    {"active": false, "windows": [window("elsewhere", 0.0, 8000.0)]},
                ]},
                {"name": "DP-1", "workspaces": [
                    {"active": true, "windows": [
                        window("between", 1600.0, 2000.0),
                        {"minimized": false, "rect": {"x": 1, "y": 2, "width": 30, "height": 40}},
                    ]},
                ]},
            ],
        });
        let got = rects(&view);
        let order: Vec<(&str, &str)> = got
            .iter()
            .map(|w| (w.title.as_str(), w.monitor.as_str()))
            .collect();
        // A window never focused (no lastFocusedAt, no title) comes last.
        assert_eq!(
            order,
            [
                ("newest", "eDP-1"),
                ("between", "DP-1"),
                ("older", "eDP-1"),
                ("", "DP-1")
            ]
        );
        assert_eq!(
            got[0].rect,
            Rect {
                x: 64.0,
                y: 114.0,
                width: 772.0,
                height: 572.0
            }
        );
        assert_eq!(got[0].last_focused_at, 3000.0);
        assert_eq!(rects(&json!({})), []);
        assert_eq!(current_monitor(&view).as_deref(), Some("DP-1"));
        assert_eq!(current_monitor(&json!({"currentMonitor": ""})), None);
        assert_eq!(current_monitor(&json!({})), None);
    }

    #[test]
    fn a_role_with_no_app_id_matches_by_title() {
        let role = |_: &str| {
            Ok(vec![Named {
                app_id: None,
                title: "fish /home".into(),
            }])
        };
        assert_eq!(pick(Some("term.main"), &windows(), role), Ok(0));
    }

    #[test]
    fn describe_names_the_app() {
        assert_eq!(describe(&seen("kitty", "fish")), "kitty: \"fish\"");
        assert_eq!(describe(&seen("", "fish")), "\"fish\"");
    }
}
