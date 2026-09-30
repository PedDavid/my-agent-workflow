// Popup listing every agent. Anchors below `anchorItem` (usually the widget).
// Click a row to focus it; small buttons close the window / forget the agent.
pragma ComponentBehavior: Bound
import QtQuick
import Quickshell

PopupWindow {
    id: panel

    // Item the popup hangs under (must live in a Quickshell window).
    property Item anchorItem: null
    property color bg: "#1a1b26"
    property color fg: "#c0caf5"
    property color dim: "#565f89"
    property color border: "#2f334d"
    property int fontPixelSize: 13

    function colorFor(a: var): color {
        switch (a.status) {
        case "needs_input":
            return "#f7768e";
        case "working":
            return "#7aa2f7";
        case "idle":
            return a.attention ? "#e0af68" : "#9ece6a";
        case "exited":
            return "#565f89";
        }
        return "#a9b1d6";
    }

    anchor.item: anchorItem
    anchor.edges: Edges.Bottom
    anchor.gravity: Edges.Bottom
    anchor.adjustment: PopupAdjustment.Slide

    implicitWidth: 460
    implicitHeight: Math.max(60, Math.min(520, list.contentHeight + 16))
    color: "transparent"

    Rectangle {
        anchors.fill: parent
        color: panel.bg
        border.color: panel.border
        radius: 8

        Text {
            anchors.centerIn: parent
            visible: DroveService.agents.length === 0
            color: panel.dim
            font.pixelSize: panel.fontPixelSize
            text: DroveService.connected ? "no agents" : "drove daemon not connected"
        }

        ListView {
            id: list
            anchors.fill: parent
            anchors.margins: 8
            spacing: 4
            clip: true
            model: DroveService.agents

            delegate: Rectangle {
                id: row
                required property var modelData
                width: ListView.view.width
                height: 44
                radius: 6
                color: hover.hovered ? "#24283b" : "transparent"
                opacity: modelData.status === "exited" ? 0.6 : 1

                HoverHandler {
                    id: hover
                }
                TapHandler {
                    onTapped: DroveService.focus(row.modelData.id)
                }

                Text {
                    id: glyph
                    anchors.left: parent.left
                    anchors.leftMargin: 8
                    anchors.verticalCenter: parent.verticalCenter
                    width: 18
                    text: DroveService.statusGlyph(row.modelData)
                    color: panel.colorFor(row.modelData)
                    font.pixelSize: panel.fontPixelSize + 3
                    font.bold: true
                }

                Column {
                    anchors.left: glyph.right
                    anchors.leftMargin: 6
                    anchors.right: buttons.left
                    anchors.rightMargin: 6
                    anchors.verticalCenter: parent.verticalCenter
                    spacing: 2

                    Row {
                        spacing: 8
                        Text {
                            text: row.modelData.name
                            color: panel.fg
                            font.pixelSize: panel.fontPixelSize
                            font.bold: true
                        }
                        Text {
                            text: row.modelData.window ? "ws " + row.modelData.window.workspace : "no window"
                            color: panel.dim
                            font.pixelSize: panel.fontPixelSize - 2
                        }
                        Text {
                            visible: row.modelData.maybe === true
                            text: "(guess)"
                            color: "#e0af68"
                            font.pixelSize: panel.fontPixelSize - 2
                        }
                        Rectangle {
                            visible: row.modelData.adopted === true
                            width: adoptedText.implicitWidth + 8
                            height: adoptedText.implicitHeight + 2
                            radius: 4
                            color: "transparent"
                            border.color: panel.dim
                            Text {
                                id: adoptedText
                                anchors.centerIn: parent
                                text: "adopted"
                                color: panel.dim
                                font.pixelSize: panel.fontPixelSize - 3
                            }
                        }
                    }
                    Text {
                        width: parent.width
                        elide: Text.ElideRight
                        text: row.modelData.detail || row.modelData.status
                        color: panel.dim
                        font.pixelSize: panel.fontPixelSize - 2
                    }
                }

                Row {
                    id: buttons
                    anchors.right: parent.right
                    anchors.rightMargin: 8
                    anchors.verticalCenter: parent.verticalCenter
                    spacing: 6

                    Rectangle {
                        visible: row.modelData.status !== "exited"
                        width: 22
                        height: 22
                        radius: 4
                        color: closeHover.hovered ? "#3b2b3a" : "#2a2e45"
                        Text {
                            anchors.centerIn: parent
                            text: "×"
                            color: "#f7768e"
                            font.pixelSize: panel.fontPixelSize + 2
                        }
                        HoverHandler {
                            id: closeHover
                        }
                        TapHandler {
                            onTapped: DroveService.close(row.modelData.id)
                        }
                    }
                    Rectangle {
                        width: 22
                        height: 22
                        radius: 4
                        color: forgetHover.hovered ? "#3b3b2b" : "#2a2e45"
                        Text {
                            anchors.centerIn: parent
                            text: "⌀"
                            color: panel.dim
                            font.pixelSize: panel.fontPixelSize
                        }
                        HoverHandler {
                            id: forgetHover
                        }
                        TapHandler {
                            onTapped: DroveService.forget(row.modelData.id)
                        }
                    }
                }
            }
        }
    }
}
