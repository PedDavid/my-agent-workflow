// Compact bar module for drove: "! 1  ◉ 2  ◐ 1  ● 3".
//   left click  -> DroveService.next()   (focus whatever needs you)
//   right click -> toggle the agent panel
// Drop it into any Quickshell bar; it sizes itself.
pragma ComponentBehavior: Bound
import QtQuick
import Quickshell

Item {
    id: root

    property alias panelOpen: panel.visible
    property color textColor: "#d8dee9"
    property color dimColor: "#6b7280"
    property color alertColor: "#f7768e"
    property color attentionColor: "#e0af68"
    property color workingColor: "#7aa2f7"
    property color idleColor: "#9ece6a"
    property int fontPixelSize: 13
    // Set false to manage a DrovePanel yourself.
    property bool embedPanel: true

    signal panelRequested

    implicitHeight: row.implicitHeight + 8
    implicitWidth: row.implicitWidth + 16

    // Pulsing red backdrop while anything needs input.
    Rectangle {
        id: pulse
        anchors.fill: parent
        radius: height / 2
        color: root.alertColor
        opacity: 0
        visible: DroveService.needsInput > 0

        SequentialAnimation on opacity {
            running: DroveService.needsInput > 0
            loops: Animation.Infinite
            NumberAnimation {
                from: 0.10
                to: 0.45
                duration: 700
                easing.type: Easing.InOutSine
            }
            NumberAnimation {
                from: 0.45
                to: 0.10
                duration: 700
                easing.type: Easing.InOutSine
            }
        }
    }

    Row {
        id: row
        anchors.centerIn: parent
        spacing: 10

        Text {
            visible: !DroveService.connected
            text: "drove ✕"
            color: root.dimColor
            font.pixelSize: root.fontPixelSize
        }

        Repeater {
            model: DroveService.connected ? [
                {
                    g: "!",
                    n: DroveService.needsInput,
                    c: root.alertColor,
                    name: "needsInput"
                },
                {
                    g: "◉",
                    n: DroveService.attention,
                    c: root.attentionColor,
                    name: "attention"
                },
                {
                    g: "◐",
                    n: DroveService.working,
                    c: root.workingColor,
                    name: "working"
                },
                {
                    g: "●",
                    n: DroveService.idle,
                    c: root.idleColor,
                    name: "idle"
                }
            ] : []

            delegate: Text {
                required property var modelData
                text: modelData.g + " " + modelData.n
                color: modelData.n > 0 ? modelData.c : root.dimColor
                font.pixelSize: root.fontPixelSize
                font.bold: modelData.name === "needsInput" && modelData.n > 0
            }
        }
    }

    MouseArea {
        anchors.fill: parent
        acceptedButtons: Qt.LeftButton | Qt.RightButton
        cursorShape: Qt.PointingHandCursor
        onClicked: mouse => {
            if (mouse.button === Qt.RightButton) {
                root.panelRequested();
                if (root.embedPanel)
                    panel.visible = !panel.visible;
            } else {
                DroveService.next();
            }
        }
    }

    DrovePanel {
        id: panel
        visible: false
        anchorItem: root
    }
}
