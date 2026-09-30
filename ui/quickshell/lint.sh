#!/usr/bin/env bash
# Static check with qmllint. Quickshell generates a qmldir for the config dir
# at runtime; qmllint needs one, so lint a temp copy that has one.
#   QS_QML=<quickshell>/lib/qt-6/qml QT_QML=<qtdeclarative>/lib/qt-6/qml ./lint.sh
set -euo pipefail
here=$(cd "$(dirname "$0")" && pwd)
: "${QS_QML:=$(dirname "$(dirname "$(readlink -f "$(command -v quickshell)")")")/lib/qt-6/qml}"
: "${QT_QML:?set QT_QML to the qtdeclarative lib/qt-6/qml}"
tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT
cp "$here"/*.qml "$tmp"/
cat >"$tmp/qmldir" <<'EOF'
singleton DroveService 1.0 DroveService.qml
DroveWidget 1.0 DroveWidget.qml
DrovePanel 1.0 DrovePanel.qml
EOF
cd "$tmp"
exec qmllint -I "$QS_QML" -I "$QT_QML" -I . "$@" DroveService.qml DroveWidget.qml DrovePanel.qml shell.qml
