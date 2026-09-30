// Shared connection to the drove daemon (see docs/protocol.md).
//
//  - `subSock` is the event connection: sends `subscribe`, keeps `agents`
//    (sorted) up to date from snapshot/agent/removed events.
//  - `reqSock` is a second connection for request/response calls (focus,
//    next, close, send_text, forget, ...). Requests made while it is down are
//    queued and flushed on connect.
//  - Both reconnect with exponential backoff (0.5s -> 5s).
pragma Singleton

import QtQuick
import Quickshell
import Quickshell.Io

Singleton {
    id: root

    // ---- configuration -------------------------------------------------
    // Override before first use if needed: DroveService.socketPath = "...".
    property string socketPath: {
        const s = Quickshell.env("DROVE_SOCKET");
        if (s)
            return s;
        const x = Quickshell.env("XDG_RUNTIME_DIR");
        if (x)
            return x + "/drove/drove.sock";
        return "/tmp/drove-" + (Quickshell.env("UID") || "0") + "/drove.sock";
    }
    property int minBackoffMs: 500
    property int maxBackoffMs: 5000

    // ---- state ---------------------------------------------------------
    property bool subscribed: false
    readonly property bool connected: subUp && subscribed
    // Sorted array of agent objects exactly as in the protocol.
    property var agents: []
    // needsInput: status needs_input. attention: attention flag set (and not
    // needs_input). working / idle: those statuses without the attention flag.
    // Exited agents are counted nowhere.
    property int needsInput: 0
    property int attention: 0
    property int working: 0
    property int idle: 0
    readonly property int total: agents.length
    property string lastError: ""

    // ---- actions -------------------------------------------------------
    function focus(id: string): void {
        request("focus", {
            id: id
        });
    }
    function next(): void {
        request("next", {});
    }
    function close(id: string): void {
        request("close", {
            id: id
        });
    }
    function forget(id: string): void {
        request("forget", {
            id: id
        });
    }
    function send(id: string, text: string, enter: bool): void {
        request("send_text", {
            id: id,
            text: text,
            enter: enter === undefined ? true : enter
        });
    }

    // Generic call. cb(ok, resultOrError) is optional.
    function request(method: string, params: var, cb: var): void {
        const id = nextReqId++;
        const line = JSON.stringify({
            id: id,
            method: method,
            params: params || {}
        }) + "\n";
        queue.push({
            id: id,
            line: line,
            cb: cb || null
        });
        flushQueue();
    }

    // Sort order from docs/protocol.md: needs_input, attention, working,
    // idle, starting, exited; ties by updated_at desc.
    function rank(a: var): int {
        if (a.status === "needs_input")
            return 0;
        if (a.status === "exited")
            return 5;
        if (a.attention)
            return 1;
        if (a.status === "working")
            return 2;
        if (a.status === "idle")
            return 3;
        if (a.status === "starting")
            return 4;
        return 6;
    }

    function statusGlyph(a: var): string {
        switch (a.status) {
        case "needs_input":
            return "!";
        case "working":
            return "◐";
        case "idle":
            return "●";
        case "starting":
            return "○";
        case "exited":
            return "×";
        }
        return "?";
    }

    // ---- internals -----------------------------------------------------
    property int nextReqId: 100
    property var queue: []      // not yet written
    property var pending: ({})  // id -> callback, written and awaiting reply
    property var byId: ({})
    property int subBackoff: minBackoffMs
    property int reqBackoff: minBackoffMs

    function rebuild(): void {
        const list = Object.values(byId);
        list.sort((a, b) => {
            const d = rank(a) - rank(b);
            return d !== 0 ? d : (b.updated_at || 0) - (a.updated_at || 0);
        });
        let n = 0, at = 0, w = 0, i = 0;
        for (const a of list) {
            if (a.status === "exited")
                continue;
            if (a.status === "needs_input")
                n++;
            else if (a.attention)
                at++;
            else if (a.status === "working")
                w++;
            else if (a.status === "idle")
                i++;
        }
        agents = list;
        needsInput = n;
        attention = at;
        working = w;
        idle = i;
    }

    function handleEvent(msg: var): void {
        if (msg.event === "snapshot") {
            const m = {};
            for (const a of msg.agents || [])
                m[a.id] = a;
            byId = m;
        } else if (msg.event === "agent" && msg.agent) {
            byId[msg.agent.id] = msg.agent;
        } else if (msg.event === "removed") {
            delete byId[msg.id];
        } else {
            return; // unknown events: ignore
        }
        rebuild();
    }

    function flushQueue(): void {
        if (!reqUp) {
            return;
        }
        while (queue.length > 0) {
            const q = queue.shift();
            pending[q.id] = q.cb;
            reqSock.write(q.line);
        }
        reqSock.flush();
    }

    function failPending(msg: string): void {
        const p = pending;
        pending = ({});
        for (const k in p)
            if (p[k])
                p[k](false, msg);
    }

    function parse(line: string): var {
        if (!line)
            return null;
        try {
            return JSON.parse(line);
        } catch (e) {
            return null;
        }
    }

    function onSubLine(line: string): void {
        const msg = parse(line);
        if (!msg)
            return;
        if (msg.event) {
            handleEvent(msg);
        } else if (msg.id === 1) {
            if (msg.ok) {
                subscribed = true;
                lastError = "";
                subBackoff = minBackoffMs;
            } else {
                lastError = msg.error || "subscribe failed";
            }
        }
    }

    function onReqLine(line: string): void {
        const msg = parse(line);
        if (!msg)
            return;
        const cb = pending[msg.id];
        delete pending[msg.id];
        if (!msg.ok)
            lastError = msg.error || "request failed";
        if (cb)
            cb(!!msg.ok, msg.ok ? msg.result : msg.error);
    }

    // Sockets live inside Loaders and are re-created for every attempt:
    // toggling `connected` on the same Socket after a failed connect does not
    // retry reliably (seen in the VM test), a fresh object does.
    readonly property var subSock: subLoader.item
    readonly property var reqSock: reqLoader.item
    readonly property bool subUp: subSock ? subSock.connected : false
    readonly property bool reqUp: reqSock ? reqSock.connected : false

    Component {
        id: subComp
        Socket {
            path: root.socketPath
            connected: true
            parser: SplitParser {
                onRead: data => root.onSubLine(data)
            }
            onConnectedChanged: {
                if (connected) {
                    write(JSON.stringify({
                        id: 1,
                        method: "subscribe"
                    }) + "\n");
                    flush();
                } else {
                    root.subscribed = false;
                    root.byId = ({});
                    root.rebuild();
                }
            }
        }
    }

    Component {
        id: reqComp
        Socket {
            path: root.socketPath
            connected: true
            parser: SplitParser {
                onRead: data => root.onReqLine(data)
            }
            onConnectedChanged: {
                if (connected) {
                    root.reqBackoff = root.minBackoffMs;
                    root.flushQueue();
                } else {
                    root.failPending("disconnected");
                }
            }
        }
    }

    Loader {
        id: subLoader
        sourceComponent: subComp
    }

    Loader {
        id: reqLoader
        sourceComponent: reqComp
    }

    // While a socket is down, replace it every `backoff` ms (0.5s -> 5s).
    Timer {
        id: subReconnect
        interval: root.subBackoff
        repeat: true
        running: !root.subUp
        onTriggered: {
            root.subBackoff = Math.min(root.subBackoff * 2, root.maxBackoffMs);
            subLoader.active = false;
            subLoader.active = true;
        }
    }

    Timer {
        id: reqReconnect
        interval: root.reqBackoff
        repeat: true
        running: !root.reqUp
        onTriggered: {
            root.reqBackoff = Math.min(root.reqBackoff * 2, root.maxBackoffMs);
            reqLoader.active = false;
            reqLoader.active = true;
        }
    }
}
