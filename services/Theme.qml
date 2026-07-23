pragma Singleton
import QtQuick
import Quickshell
// Through the config-root symlink: Quickshell only honours qmldir
// singleton registration for paths inside the shell root.
import "../Proustite"

// Thin facade over the shared Proustite palette, plus MinkaShot's
// capture-overlay extras.
// Widgets style through Theme only.
Singleton {
    readonly property color ground: Proustite.ground
    readonly property color surface: Proustite.surface
    readonly property color surfaceRaised: Proustite.surfaceRaised
    readonly property color line: Proustite.line
    readonly property color text: Proustite.text
    readonly property color textMuted: Proustite.textMuted
    readonly property color textFaint: Proustite.textFaint
    readonly property color red: Proustite.red
    readonly property color redDim: Proustite.redDim
    readonly property color purple: Proustite.purple
    readonly property color okGreen: Proustite.okGreen
    readonly property color warnAmber: Proustite.warnAmber

    // Scrim over the parts of a frozen frame outside the selected region.
    readonly property color scrim: Qt.rgba(
        Proustite.ground.r, Proustite.ground.g, Proustite.ground.b, 0.64)

    readonly property string fontFamily: Proustite.fontFamily
    readonly property string monoFamily: Proustite.monoFamily
    readonly property int fontSize: Proustite.fontSize
}