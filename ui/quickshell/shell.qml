// Standalone demo bar:  quickshell -p ui/quickshell/shell.qml
// Also exposes `qs -p ui/quickshell/shell.qml ipc call drove <fn>` for scripting/tests.
pragma ComponentBehavior: Bound
import QtQuick
import Quickshell
import Quickshell.Io

ShellRoot {
    id: root

    signal togglePanel

    Variants {
        model: Quickshell.screens

        PanelWindow {
            id: bar
            required property var modelData
            screen: modelData
            anchors {
                top: true
                left: true
                right: true
            }
            implicitHeight: 32
            color: "#1a1b26"

            Text {
                anchors.left: parent.left
                anchors.leftMargin: 12
                anchors.verticalCenter: parent.verticalCenter
                text: "drove demo bar"
                color: "#565f89"
                font.pixelSize: 13
            }

            DroveWidget {
                id: widget
                anchors.right: parent.right
                anchors.rightMargin: 12
                anchors.verticalCenter: parent.verticalCenter
            }

            Connections {
                target: root
                function onTogglePanel() {
                    widget.panelOpen = !widget.panelOpen;
                }
            }
        }
    }

    IpcHandler {
        target: "drove"
        function next(): void {
            DroveService.next();
        }
        function focus(id: string): void {
            DroveService.focus(id);
        }
        function close(id: string): void {
            DroveService.close(id);
        }
        function send(id: string, text: string): void {
            DroveService.send(id, text, true);
        }
        function togglePanel(): void {
            root.togglePanel();
        }
        // "connected needsInput attention working idle total first-agent-name"
        function state(): string {
            const a = DroveService.agents;
            return [DroveService.connected, DroveService.needsInput, DroveService.attention, DroveService.working, DroveService.idle, a.length, a.length ? a[0].name : "-"].join(" ");
        }
    }
}
