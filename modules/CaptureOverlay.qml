import Quickshell
import Quickshell.Wayland
import QtQuick
import "../services"

// Per-screen frozen-frame capture surface. The ScreencopyView freezes this
// output the moment the overlay maps (capture fires before content exists,
// so the overlay itself is never in the shot); everything drawn above it —
// crosshair, loupe, scrim, toolbar — is UI chrome that saves can't see,
// because saves grab the view through a ShaderEffectSource, not the window.
PanelWindow {
    id: overlay

    required property var modelData

    readonly property bool involved: ShotState.armed
        && (!ShotState.silent
            || (ShotState.job !== null
                && ShotState.job.screenName === modelData.name))
    readonly property bool interactiveMode: involved && !ShotState.silent
    readonly property bool focused: interactiveMode
        && ShotState.activeScreen === modelData.name

    // Selection state, screen-local logical coords.
    property var regionRect: null
    property var dragOrigin: null
    property point cursor: Qt.point(-1, -1)
    property var hoverWindow: null

    screen: modelData
    visible: involved
    color: "transparent"
    exclusionMode: ExclusionMode.Ignore
    WlrLayershell.layer: WlrLayer.Overlay
    WlrLayershell.namespace: "minkashot"
    WlrLayershell.keyboardFocus: focused
        ? WlrKeyboardFocus.Exclusive : WlrKeyboardFocus.None
    // Silent captures must not eat a single click.
    mask: ShotState.silent ? passMask : null

    anchors {
        top: true
        bottom: true
        left: true
        right: true
    }

    Region {
        id: passMask
    }

    onVisibleChanged: {
        if (visible) {
            regionRect = null;
            dragOrigin = null;
            hoverWindow = null;
            cursor = Qt.point(-1, -1);
            view.captureFrame();
        }
    }

    function normalized(a, b) {
        return {
            x: Math.min(a.x, b.x),
            y: Math.min(a.y, b.y),
            width: Math.abs(b.x - a.x),
            height: Math.abs(b.y - a.y),
        };
    }

    // Topmost freeze-time window under the point, in local coords.
    function windowAt(x, y) {
        const gx = modelData.x + x;
        const gy = modelData.y + y;
        for (const w of ShotState.windowRects) {
            if (w.monitor !== modelData.name)
                continue;
            if (gx >= w.x && gx < w.x + w.width
                && gy >= w.y && gy < w.y + w.height) {
                return {
                    x: w.x - modelData.x,
                    y: w.y - modelData.y,
                    width: w.width,
                    height: w.height,
                    title: w.title,
                };
            }
        }
        return null;
    }

    // Crop `rect` (null = whole screen) out of the frozen frame at native
    // buffer resolution and write it to `path`.
    function performSave(rect, path) {
        const r = rect === null
            ? { x: 0, y: 0, width: view.width, height: view.height }
            : rect;
        const sx = view.sourceSize.width / view.width;
        const sy = view.sourceSize.height / view.height;
        const pw = Math.max(1, Math.round(r.width * sx));
        const ph = Math.max(1, Math.round(r.height * sy));
        grabSource.sourceRect = Qt.rect(r.x, r.y, r.width, r.height);
        grabSource.textureSize = Qt.size(pw, ph);
        grabSource.width = r.width;
        grabSource.height = r.height;
        grabSource.scheduleUpdate();
        const target = path !== undefined && path !== null && path.length > 0
            ? path : ShotState.defaultPath();
        grabSource.grabToImage(result => {
            if (result.saveToFile(target))
                console.log("minkashot: saved", target);
            else
                console.error("minkashot: failed to save", target);
            ShotState.disarm();
        }, Qt.size(pw, ph));
    }

    function maybeRunJob() {
        if (!ShotState.silent || ShotState.job === null || !view.hasContent)
            return;
        if (ShotState.job.screenName !== modelData.name)
            return;
        jobSettle.restart();
    }

    Connections {
        target: ShotState

        function onJobChanged() {
            overlay.maybeRunJob();
        }
    }

    // A beat between "frame arrived" and the grab, so the texture upload
    // definitely landed.
    Timer {
        id: jobSettle

        interval: 150
        onTriggered: {
            if (ShotState.job !== null)
                overlay.performSave(ShotState.job.rect, ShotState.job.path);
        }
    }

    Item {
        id: content

        anchors.fill: parent
        focus: overlay.focused

        Keys.onPressed: event => {
            if (event.key === Qt.Key_Escape) {
                ShotState.disarm();
                event.accepted = true;
            } else if (event.key === Qt.Key_Return
                       || event.key === Qt.Key_Enter) {
                overlay.performSave(overlay.regionRect, "");
                event.accepted = true;
            }
        }

        // Save source, behind the view: "visible" to the scene graph so
        // grabToImage can render it, occluded by the identical frozen frame
        // above so the user never sees it.
        ShaderEffectSource {
            id: grabSource

            z: -1
            width: 1
            height: 1
            sourceItem: view
            live: false
            recursive: false
        }

        ScreencopyView {
            id: view

            anchors.fill: parent
            captureSource: overlay.modelData
            live: false
            paintCursor: false

            onHasContentChanged: overlay.maybeRunJob()
        }

        MouseArea {
            id: area

            anchors.fill: parent
            enabled: overlay.interactiveMode
            hoverEnabled: true
            // The loupe crosshair replaces the pointer entirely.
            cursorShape: overlay.interactiveMode
                ? Qt.BlankCursor : Qt.ArrowCursor
            acceptedButtons: Qt.LeftButton

            onPositionChanged: mouse => {
                overlay.cursor = Qt.point(mouse.x, mouse.y);
                ShotState.activeScreen = overlay.modelData.name;
                if (overlay.dragOrigin !== null)
                    overlay.regionRect = overlay.normalized(
                        overlay.dragOrigin, overlay.cursor);
                if (ShotState.pickMode)
                    overlay.hoverWindow = overlay.windowAt(mouse.x, mouse.y);
                else if (overlay.hoverWindow !== null)
                    overlay.hoverWindow = null;
            }
            onPressed: mouse => {
                if (ShotState.pickMode) {
                    const w = overlay.windowAt(mouse.x, mouse.y);
                    if (w !== null) {
                        overlay.regionRect = w;
                        overlay.hoverWindow = null;
                        ShotState.pickMode = false;
                    }
                    return;
                }
                overlay.dragOrigin = Qt.point(mouse.x, mouse.y);
                overlay.regionRect = null;
            }
            onReleased: {
                if (overlay.regionRect !== null
                    && (overlay.regionRect.width < 4
                        || overlay.regionRect.height < 4))
                    overlay.regionRect = null;
                overlay.dragOrigin = null;
            }
            onExited: overlay.cursor = Qt.point(-1, -1)
        }

        // Scrim outside the selected region, ksnip-style.
        Item {
            anchors.fill: parent
            visible: overlay.interactiveMode && overlay.regionRect !== null

            Rectangle {
                x: 0
                y: 0
                width: parent.width
                height: overlay.regionRect !== null ? overlay.regionRect.y : 0
                color: Theme.scrim
            }

            Rectangle {
                x: 0
                y: overlay.regionRect !== null
                    ? overlay.regionRect.y + overlay.regionRect.height : 0
                width: parent.width
                height: parent.height - y
                color: Theme.scrim
            }

            Rectangle {
                x: 0
                y: overlay.regionRect !== null ? overlay.regionRect.y : 0
                width: overlay.regionRect !== null ? overlay.regionRect.x : 0
                height: overlay.regionRect !== null
                    ? overlay.regionRect.height : 0
                color: Theme.scrim
            }

            Rectangle {
                x: overlay.regionRect !== null
                    ? overlay.regionRect.x + overlay.regionRect.width : 0
                y: overlay.regionRect !== null ? overlay.regionRect.y : 0
                width: parent.width - x
                height: overlay.regionRect !== null
                    ? overlay.regionRect.height : 0
                color: Theme.scrim
            }
        }

        // Region outline + dimensions readout.
        Rectangle {
            visible: overlay.interactiveMode && overlay.regionRect !== null
            x: overlay.regionRect !== null ? overlay.regionRect.x : 0
            y: overlay.regionRect !== null ? overlay.regionRect.y : 0
            width: overlay.regionRect !== null ? overlay.regionRect.width : 0
            height: overlay.regionRect !== null ? overlay.regionRect.height : 0
            color: "transparent"
            border.width: 1
            border.color: Theme.red

            Text {
                anchors.top: parent.bottom
                anchors.right: parent.right
                anchors.topMargin: 4
                text: Math.round(parent.width) + "×" + Math.round(parent.height)
                font.family: Theme.monoFamily
                font.pixelSize: Theme.fontSize - 2
                color: Theme.red
            }
        }

        // Window-pick hover highlight.
        Rectangle {
            visible: overlay.hoverWindow !== null
            x: overlay.hoverWindow !== null ? overlay.hoverWindow.x : 0
            y: overlay.hoverWindow !== null ? overlay.hoverWindow.y : 0
            width: overlay.hoverWindow !== null ? overlay.hoverWindow.width : 0
            height: overlay.hoverWindow !== null
                ? overlay.hoverWindow.height : 0
            color: Theme.red
            opacity: 0.12
        }

        Rectangle {
            visible: overlay.hoverWindow !== null
            x: overlay.hoverWindow !== null ? overlay.hoverWindow.x : 0
            y: overlay.hoverWindow !== null ? overlay.hoverWindow.y : 0
            width: overlay.hoverWindow !== null ? overlay.hoverWindow.width : 0
            height: overlay.hoverWindow !== null
                ? overlay.hoverWindow.height : 0
            color: "transparent"
            border.width: 1
            border.color: Theme.red

            Text {
                anchors.bottom: parent.top
                anchors.left: parent.left
                anchors.bottomMargin: 4
                text: overlay.hoverWindow !== null ? overlay.hoverWindow.title : ""
                font.family: Theme.monoFamily
                font.pixelSize: Theme.fontSize - 2
                color: Theme.red
            }
        }

        // Full-span crosshair through the pointer.
        Rectangle {
            visible: overlay.interactiveMode && view.hasContent
                && overlay.cursor.x >= 0
            x: 0
            y: overlay.cursor.y
            width: parent.width
            height: 1
            color: Theme.red
            opacity: 0.35
        }

        Rectangle {
            visible: overlay.interactiveMode && view.hasContent
                && overlay.cursor.x >= 0
            x: overlay.cursor.x
            y: 0
            width: 1
            height: parent.height
            color: Theme.red
            opacity: 0.35
        }

        // The magnified loupe: 21×21 logical pixels around the pointer at
        // 6× zoom, nearest-neighbour so cells stay crisp.
        Item {
            id: loupe

            readonly property int cells: 21
            readonly property int cellPx: 6
            readonly property int body: cells * cellPx

            visible: overlay.interactiveMode && view.hasContent
                && overlay.cursor.x >= 0
            width: body + 2
            height: body + 20
            x: overlay.cursor.x + 24 + width > parent.width
                ? overlay.cursor.x - 24 - width
                : overlay.cursor.x + 24
            y: overlay.cursor.y + 24 + height > parent.height
                ? overlay.cursor.y - 24 - height
                : overlay.cursor.y + 24

            Rectangle {
                anchors.fill: parent
                color: Theme.ground
                border.width: 1
                border.color: Theme.red
            }

            ShaderEffectSource {
                x: 1
                y: 1
                width: loupe.body
                height: loupe.body
                sourceItem: view
                sourceRect: Qt.rect(
                    Math.floor(overlay.cursor.x) - 10,
                    Math.floor(overlay.cursor.y) - 10,
                    loupe.cells, loupe.cells)
                smooth: false
            }

            // The pixel under the pointer.
            Rectangle {
                x: 1 + 10 * loupe.cellPx
                y: 1 + 10 * loupe.cellPx
                width: loupe.cellPx
                height: loupe.cellPx
                color: "transparent"
                border.width: 1
                border.color: Theme.red
            }

            Text {
                anchors.bottom: parent.bottom
                anchors.bottomMargin: 3
                anchors.horizontalCenter: parent.horizontalCenter
                text: Math.floor(overlay.cursor.x) + ", "
                    + Math.floor(overlay.cursor.y)
                    + (overlay.regionRect !== null
                        ? "  ·  " + Math.round(overlay.regionRect.width)
                            + "×" + Math.round(overlay.regionRect.height)
                        : "")
                font.family: Theme.monoFamily
                font.pixelSize: Theme.fontSize - 4
                color: Theme.textMuted
            }
        }

        ControlPanel {
            visible: overlay.interactiveMode && view.hasContent
            anchors.horizontalCenter: parent.horizontalCenter
            y: 28
        }
    }
}