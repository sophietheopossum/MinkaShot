pragma Singleton
import Quickshell
import QtQuick
// Through the config-root symlink: Quickshell only honours qmldir
// singleton registration for paths inside the shell root.
import "../MinkaLink"

// MinkaShot's capture state machine. Interactive flow: freeze every output,
// loupe active, Enter saves (region if drawn, else the pointed screen), Esc
// cancels; retry re-freezes after the configured delay. Silent flow
// (scripted via the shot IPC target): a job names one screen and an
// optional region; its overlay maps input-transparent, saves, disarms.
Singleton {
    id: root

    property bool armed: false
    // Scripted capture: no UI chrome, no input grab.
    property bool silent: false
    property bool pickMode: false
    property int delaySecs: 0
    property int countdown: 0
    // The overlay under the pointer: keyboard focus and the countdown pill
    // live there.
    property string activeScreen: Quickshell.screens.length > 0
        ? Quickshell.screens[0].name : ""
    // { screenName, rect: {x,y,width,height}|null, path } in screen-local
    // logical coords; null rect = whole screen.
    property var job: null
    // Freeze-time window rects (global logical coords, halo stripped).
    property var windowRects: []
    // Screencopy photographs the composited screen, so a window capture
    // must raise its target first or the crop bakes in whatever sat above
    // it. These carry the raise dance: the job waiting for the restack to
    // settle, and the previously-focused window to hand focus back to.
    property var pendingRaiseJob: null
    property var restoreFocusId: null

    // ShojiWM window rects include the 14px edge-drag halo ring; saves and
    // pick targets want the visible border instead.
    readonly property int chromeInset: 14
    readonly property string saveDir:
        Quickshell.env("HOME") + "/Pictures/Screenshots"

    function defaultPath() {
        return saveDir + "/minkashot-"
            + Qt.formatDateTime(new Date(), "yyyyMMdd-hhmmss") + ".png";
    }

    function interactive() {
        if (armed && !silent)
            return;
        disarm();
        refreshWindows();
        armed = true;
    }

    // Teardown without side effects — used both when a job finishes and
    // when a new one starts.
    function reset() {
        countdownTimer.stop();
        settleTimer.stop();
        raiseSettle.stop();
        armed = false;
        silent = false;
        pickMode = false;
        job = null;
        pendingRaiseJob = null;
        countdown = 0;
    }

    function disarm() {
        reset();
        // If a window capture raised its target, hand focus back. Runs on
        // save completion, cancel, and the watchdog alike.
        if (restoreFocusId !== null) {
            ShojiClient.send("windows.activate", {
                windowId: restoreFocusId,
            });
            restoreFocusId = null;
        }
    }

    // Re-freeze after the configured delay. Overlays unmap immediately and
    // the settle timer keeps them unmapped long enough that the old frozen
    // frame can never appear inside the new capture.
    function retry() {
        armed = false;
        pickMode = false;
        if (delaySecs > 0) {
            countdown = delaySecs;
            countdownTimer.start();
        } else {
            settleTimer.restart();
        }
    }

    function refreshWindows() {
        ShojiClient.request("workspaces.get", undefined, (result, error) => {
            if (!result || !result.monitors)
                return;
            const out = [];
            for (const mon of result.monitors) {
                for (const ws of mon.workspaces) {
                    if (!ws.active)
                        continue;
                    for (const w of ws.windows) {
                        if (!w.rect || w.minimized)
                            continue;
                        out.push({
                            title: w.title || "",
                            monitor: mon.name,
                            lastFocusedAt: w.lastFocusedAt || 0,
                            x: w.rect.x + root.chromeInset,
                            y: w.rect.y + root.chromeInset,
                            width: w.rect.width - root.chromeInset * 2,
                            height: w.rect.height - root.chromeInset * 2,
                        });
                    }
                }
            }
            // Top-first so pick hits resolve like stacking (approximated by
            // focus recency, same as MinkaMon's occlusion pass).
            out.sort((a, b) => b.lastFocusedAt - a.lastFocusedAt);
            root.windowRects = out;
        });
    }

    function resolveScreen(name) {
        for (const s of Quickshell.screens) {
            if (s.name === name)
                return name;
        }
        return Quickshell.screens.length > 0 ? Quickshell.screens[0].name : "";
    }

    // Order matters: overlays react to jobChanged, and their check reads
    // `silent` — it must already be true when the job lands (the 23/7
    // frozen-screen incident was this exact race, inverted).
    function startJob(j) {
        disarm();
        silent = true;
        job = j;
        armed = true;
    }

    function fullJob(screenName, path) {
        startJob({ screenName: resolveScreen(screenName), rect: null, path });
    }

    function regionJob(screenName, spec, path) {
        const parts = spec.split(",").map(Number);
        if (parts.length !== 4 || parts.some(isNaN)) {
            console.error("minkashot: bad region spec:", spec,
                "(want \"x,y,w,h\")");
            return;
        }
        startJob({
            screenName: resolveScreen(screenName),
            rect: {
                x: parts[0],
                y: parts[1],
                width: parts[2],
                height: parts[3],
            },
            path,
        });
    }

    // `selector` matches a claimed semantic role ("minkamon.disk") first,
    // falling back to exact title for windows that never claimed one.
    function windowJob(
        selector, 
        path,
    ) {
        ShojiClient.request("workspaces.get", undefined, (result, error) => {
            if (!result || !result.monitors) {
                console.error("minkashot: no workspace view for win capture");
                return;
            }
            let focusedId = null;
            for (const mon of result.monitors)
                for (const ws of mon.workspaces)
                    for (const w of ws.windows)
                        if (ws.active && w.focused)
                            focusedId = w.id;
            for (const mon of result.monitors) {
                for (const ws of mon.workspaces) {
                    if (!ws.active)
                        continue;
                    for (const w of ws.windows) {
                        if (w.minimized || !w.rect
                            || (
                                w.role !== selector && w.title !== selector
                            )
                        )
                            continue;
                        const screen = Quickshell.screens.find(
                            s => s.name === mon.name);
                        if (!screen)
                            continue;
                        const job = {
                            screenName: mon.name,
                            rect: {
                                x: w.rect.x + root.chromeInset - screen.x,
                                y: w.rect.y + root.chromeInset - screen.y,
                                width: w.rect.width - root.chromeInset * 2,
                                height: w.rect.height - root.chromeInset * 2,
                            },
                            path,
                        };
                        if (w.focused) {
                            // Already on top: freeze straight away.
                            root.startJob(job);
                        } else {
                            // Raise it so the crop shows the window, not
                            // whatever was stacked above it; capture once
                            // the restack has reached the screen, then
                            // disarm() hands focus back.
                            root.restoreFocusId = focusedId;
                            ShojiClient.send("windows.activate", {
                                windowId: w.id,
                            });
                            root.pendingRaiseJob = job;
                            raiseSettle.restart();
                        }
                        return;
                    }
                }
            }
            console.error(
                "minkashot: no window matching",
                selector,
            );
        });
    }

    // Restack-to-screen settle: the compositor needs a beat to raise the
    // target and repaint before the freeze fires.
    Timer {
        id: raiseSettle

        interval: 350
        onTriggered: {
            const j = root.pendingRaiseJob;
            if (j !== null)
                root.startJob(j);
        }
    }

    // The compositor's Print keybind arrives as a broadcast.
    Connections {
        target: ShojiClient

        function onBroadcast(name, payload) {
            if (name === "ui.minkashot")
                root.interactive();
        }
    }

    Timer {
        id: countdownTimer

        interval: 1000
        repeat: true
        onTriggered: {
            root.countdown--;
            if (root.countdown <= 0) {
                stop();
                settleTimer.restart();
            }
        }
    }

    // Unmap settle: overlays and the countdown pill must be off screen (and
    // the compositor must have rendered without them) before recapturing.
    Timer {
        id: settleTimer

        interval: 250
        onTriggered: {
            root.refreshWindows();
            root.armed = true;
        }
    }

    // Hard guarantee against the 23/7 frozen-screen incident: a silent job
    // that hasn't saved and disarmed within the timeout is aborted rather
    // than left holding a stale frozen frame over the user's screen. The
    // overlay is input-transparent in silent mode, so a wedged one is worse
    // than invisible — the user interacts blind with windows they can't
    // see.
    Timer {
        interval: 6000
        running: root.armed && root.silent
        onTriggered: {
            console.error("minkashot: silent job timed out, disarming");
            root.disarm();
        }
    }

    Component.onCompleted: Quickshell.execDetached(["mkdir", "-p", saveDir])
}