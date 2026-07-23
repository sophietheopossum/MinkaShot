import Quickshell
import Quickshell.Wayland
import QtQuick
import "../services"

// Post-retry countdown pill on the pointed screen. It unmaps at zero and
// ShotState waits a settle interval before recapturing, so the pill can
// never appear in the shot it announces.
PanelWindow {
    id: chip

    required property var modelData

    screen: modelData
    visible: ShotState.countdown > 0
        && ShotState.activeScreen === modelData.name
    color: "transparent"
    exclusionMode: ExclusionMode.Ignore
    WlrLayershell.layer: WlrLayer.Overlay
    WlrLayershell.namespace: "minkashot-countdown"
    anchors.top: true
    margins.top: 120
    implicitWidth: label.implicitWidth + 36
    implicitHeight: 40
    mask: Region {}

    Rectangle {
        anchors.fill: parent
        radius: 8
        color: Theme.surface
        border.width: 1
        border.color: Theme.redDim

        Text {
            id: label

            anchors.centerIn: parent
            text: "⧗ " + ShotState.countdown
            font.family: Theme.monoFamily
            font.pixelSize: Theme.fontSize + 2
            color: Theme.red
        }
    }
}