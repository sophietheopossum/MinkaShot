import QtQuick
import "../services"

// The floating capture toolbar: delay stepper, window pick, retry. Sits on
// the frozen frame; it can never appear in a save because saves grab the
// ScreencopyView, not the overlay UI.
Rectangle {
    id: panel

    implicitWidth: row.implicitWidth + 28
    implicitHeight: 40
    radius: 7
    color: Theme.surface
    border.width: 1
    border.color: Theme.line

    component Chip: Rectangle {
        id: chip

        property string label
        property bool active: false

        signal clicked()

        width: chipText.implicitWidth + 18
        height: 26
        radius: 5
        color: active ? Theme.surfaceRaised : "transparent"
        border.width: 1
        border.color: active ? Theme.red : Theme.line
        opacity: enabled ? 1 : 0.4

        Text {
            id: chipText

            anchors.centerIn: parent
            text: chip.label
            font.family: Theme.monoFamily
            font.pixelSize: Theme.fontSize - 2
            color: chip.active ? Theme.text : Theme.textMuted
        }

        MouseArea {
            anchors.fill: parent
            onClicked: chip.clicked()
        }
    }

    Row {
        id: row

        anchors.centerIn: parent
        spacing: 8

        Text {
            anchors.verticalCenter: parent.verticalCenter
            text: "MINKASHOT"
            font.family: Theme.monoFamily
            font.pixelSize: Theme.fontSize - 1
            font.letterSpacing: 2
            font.bold: true
            color: Theme.red
        }

        Text {
            anchors.verticalCenter: parent.verticalCenter
            text: "DELAY"
            font.family: Theme.monoFamily
            font.pixelSize: Theme.fontSize - 3
            font.letterSpacing: 1
            color: Theme.textFaint
        }

        Chip {
            anchors.verticalCenter: parent.verticalCenter
            label: "−"
            enabled: ShotState.delaySecs > 0
            onClicked: ShotState.delaySecs--
        }

        Text {
            anchors.verticalCenter: parent.verticalCenter
            text: ShotState.delaySecs + "s"
            font.family: Theme.monoFamily
            font.pixelSize: Theme.fontSize - 1
            color: ShotState.delaySecs > 0 ? Theme.text : Theme.textFaint
        }

        Chip {
            anchors.verticalCenter: parent.verticalCenter
            label: "+"
            enabled: ShotState.delaySecs < 10
            onClicked: ShotState.delaySecs++
        }

        Chip {
            anchors.verticalCenter: parent.verticalCenter
            label: "WINDOW"
            active: ShotState.pickMode
            onClicked: ShotState.pickMode = !ShotState.pickMode
        }

        Chip {
            anchors.verticalCenter: parent.verticalCenter
            label: "↻"
            onClicked: ShotState.retry()
        }

        Text {
            anchors.verticalCenter: parent.verticalCenter
            text: "enter save · esc cancel · drag region"
            font.family: Theme.fontFamily
            font.pixelSize: Theme.fontSize - 3
            color: Theme.textFaint
        }
    }
}