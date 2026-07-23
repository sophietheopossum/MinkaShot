pragma Singleton
import QtQuick
import Quickshell
import "../../Proustite"

// Thin facade over the shared Proustite palette, plus MinkaShot's
// capture-overlay extras.
// Widgets style through Theme only.
Singleton {
    readonly property color ground: Palette.ground
    readonly property color surface: Palette.surface
    readonly property color surfaceRaised: Palette.surfaceRaised
    readonly property color line: Palette.line
    readonly property color text: Palette.text
    readonly property color textMuted: Palette.textMuted
    readonly property color textFaint: Palette.textFaint
    readonly property color red: Palette.red
    readonly property color redDim: Palette.redDim
    readonly property color purple: Palette.purple
    readonly property color okGreen: Palette.okGreen
    readonly property color warnAmber: Palette.warnAmber

    // Scrim over the parts of a frozen frame outside the selected region.
    readonly property color scrim: Qt.rgba(
        Palette.ground.r, Palette.ground.g, Palette.ground.b, 0.64)

    readonly property string fontFamily: Palette.fontFamily
    readonly property string monoFamily: Palette.monoFamily
    readonly property int fontSize: Palette.fontSize
}