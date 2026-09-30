#!/usr/bin/env python3
"""Mock drove daemon for UI development (see docs/protocol.md).

Speaks the real NDJSON protocol on a unix socket with a handful of fake
agents that cycle through states, so bars/TUIs/web pages/notifiers can be
built and tested without Hyprland, kitty or real agents.

    python3 tools/mock-drove.py --socket /tmp/drove-mock.sock [--tick 3] [--seed 1] [--log requests.log]

Every request is appended to --log (one JSON line) so tests can assert that a
UI e.g. sent {"method":"focus","params":{"id":"…"}} after a click.
Pure stdlib; Python ≥ 3.9.
"""
import argparse
import asyncio
import json
import os
import random
import string
import time

STATUSES = ["starting", "idle", "working", "needs_input", "exited"]
DETAILS = {
    "working": ["Bash: cargo test", "Read: src/main.rs", "Edit: README.md", "thinking…", "fs_read"],
    "needs_input": ["Claude needs your permission to use Bash", "Allow this action? [y/n/t]", "Question: which branch?"],
    "idle": ["done: summarized the repo", "", "waiting for prompt"],
}


def now_ms() -> int:
    return int(time.time() * 1000)


def new_id(rng: random.Random) -> str:
    return "".join(rng.choice("abcdefghijklmnopqrstuvwxyz234567") for _ in range(6))


class Mock:
    def __init__(self, seed: int, log_path):
        self.rng = random.Random(seed)
        self.agents: dict = {}
        self.focused = None
        self.subscribers: set = set()
        self.log_path = log_path
        for name, kind, ws in [
            ("api-refactor", "claude", "1"),
            ("fix-flaky-tests", "codex", "2"),
            ("infra-docs", "kiro", "3"),
            ("scratch", "claude", "3"),
        ]:
            self._add(name, kind, ws)
        a = list(self.agents.values())
        a[0].update(status="working", detail="Bash: cargo test")
        a[1].update(status="needs_input", attention=True, detail=DETAILS["needs_input"][0])
        a[2].update(status="idle", attention=True, detail="done: summarized the repo")
        a[3].update(status="idle")

    def _add(self, name, kind, ws, cwd=None, adopted=False):
        aid = new_id(self.rng)
        t = now_ms()
        addr = "0x%x" % self.rng.randrange(0x500000000000, 0x5fffffffffff)
        self.agents[aid] = {
            "id": aid, "name": name, "kind": kind, "profile": kind,
            "cwd": cwd or f"/home/me/src/{name}",
            "status": "starting", "attention": False, "detail": "",
            "session_id": None,
            "window": {"address": addr, "workspace": ws, "title": f"{kind}: {name}", "class": f"drove-{aid}"},
            "term": {"type": "kitty", "socket": f"unix:/run/user/1000/drove/kitty-{aid}.sock"},
            "worktree": None, "adopted": adopted,
            "created_at": t, "updated_at": t, "last_hook_at": None,
        }
        return self.agents[aid]

    # ---------- events ----------
    async def emit(self, ev: dict):
        line = (json.dumps(ev) + "\n").encode()
        for w in list(self.subscribers):
            try:
                w.write(line)
                await w.drain()
            except Exception:
                self.subscribers.discard(w)

    async def changed(self, a: dict):
        a["updated_at"] = now_ms()
        a.pop("maybe", None) if not a.get("maybe") else None
        await self.emit({"event": "agent", "agent": a})

    # ---------- simulation ----------
    async def tick(self):
        live = [a for a in self.agents.values() if a["status"] != "exited"]
        if not live:
            return
        a = self.rng.choice(live)
        s = a["status"]
        if s == "starting":
            a["status"] = "idle"
        elif s == "idle":
            a["status"] = "working"
            a["detail"] = self.rng.choice(DETAILS["working"])
            a["attention"] = False if self.focused == a["id"] else a["attention"]
        elif s == "working":
            r = self.rng.random()
            if r < 0.25:
                a["status"] = "needs_input"
                a["detail"] = self.rng.choice(DETAILS["needs_input"])
                if a["kind"] == "kiro":
                    a["maybe"] = True
                a["attention"] = self.focused != a["id"]
            elif r < 0.75:
                a["status"] = "idle"
                a["detail"] = self.rng.choice(DETAILS["idle"])
                a["attention"] = self.focused != a["id"]
            else:
                a["detail"] = self.rng.choice(DETAILS["working"])
        elif s == "needs_input":
            a["status"] = "working"
            a.pop("maybe", None)
            a["detail"] = self.rng.choice(DETAILS["working"])
        a["last_hook_at"] = now_ms()
        await self.changed(a)

    # ---------- requests ----------
    def resolve(self, key) -> dict:
        if not isinstance(key, str) or not key:
            raise ValueError("missing agent id")
        if key in self.agents:
            return self.agents[key]
        by_name = [a for a in self.agents.values() if a["name"] == key]
        if len(by_name) == 1:
            return by_name[0]
        pref = [a for a in self.agents.values() if a["id"].startswith(key)]
        if len(pref) == 1:
            return pref[0]
        raise ValueError(f"no agent matching {key!r}")

    def sorted_agents(self):
        rank = {"needs_input": 0, "idle": 2, "working": 3, "starting": 4, "exited": 5}
        def key(a):
            r = rank[a["status"]]
            if a["status"] == "idle" and a["attention"]:
                r = 1
            return (r, -a["updated_at"])
        return sorted(self.agents.values(), key=key)

    async def focus(self, a):
        if not a.get("window"):
            raise ValueError(f"{a['name']} has no window")
        self.focused = a["id"]
        if a["attention"]:
            a["attention"] = False
            await self.changed(a)

    async def handle(self, method: str, p: dict):
        if method == "ping":
            return "pong"
        if method == "status":
            return {"version": "mock", "pid": os.getpid(), "socket": SOCKET, "state": None,
                    "hyprland": True, "dispatch": "lua", "agents": len(self.agents)}
        if method == "list":
            return self.sorted_agents()
        if method == "get":
            return self.resolve(p.get("id"))
        if method == "spawn":
            profile = p.get("profile") or "claude"
            a = self._add(p.get("name") or f"{profile}-{len(self.agents)+1}", profile if profile in ("claude", "codex", "kiro") else "generic",
                          p.get("workspace") or "1", cwd=p.get("cwd"))
            await self.changed(a)
            return a
        if method == "focus":
            a = self.resolve(p.get("id"))
            await self.focus(a)
            return a
        if method == "next":
            for a in self.sorted_agents():
                if a["status"] == "needs_input" or a["attention"]:
                    await self.focus(a)
                    return a
            return None
        if method == "close":
            a = self.resolve(p.get("id"))
            a["status"] = "exited"
            a["window"] = None
            a["attention"] = False
            await self.changed(a)
            return None
        if method == "send_text":
            a = self.resolve(p.get("id"))
            if a["status"] == "exited":
                raise ValueError(f"{a['name']} has no terminal handle")
            a["status"] = "working"
            a["detail"] = "prompt: " + str(p.get("text", ""))[:40]
            await self.changed(a)
            return None
        if method == "get_text":
            a = self.resolve(p.get("id"))
            return f"{a['kind']} — {a['name']}\n\n> {a['detail']}\n\n[{a['status']}]\n"
        if method == "rename":
            a = self.resolve(p.get("id"))
            if not p.get("name"):
                raise ValueError("missing name")
            a["name"] = p["name"]
            await self.changed(a)
            return None
        if method == "forget":
            if p.get("exited"):
                gone = [i for i, a in self.agents.items() if a["status"] == "exited"]
            else:
                gone = [self.resolve(p.get("id"))["id"]]
            for i in gone:
                del self.agents[i]
                await self.emit({"event": "removed", "id": i})
            return len(gone)
        raise ValueError(f"unknown method {method!r}")

    async def client(self, reader, writer):
        try:
            while True:
                line = await reader.readline()
                if not line:
                    break
                try:
                    req = json.loads(line)
                except Exception as e:
                    writer.write((json.dumps({"id": None, "ok": False, "error": f"bad request: {e}"}) + "\n").encode())
                    continue
                if self.log_path:
                    with open(self.log_path, "a") as f:
                        f.write(json.dumps(req) + "\n")
                rid, method, params = req.get("id"), req.get("method", ""), req.get("params") or {}
                if method == "subscribe":
                    writer.write((json.dumps({"id": rid, "ok": True, "result": None}) + "\n").encode())
                    writer.write((json.dumps({"event": "snapshot", "agents": self.sorted_agents()}) + "\n").encode())
                    await writer.drain()
                    self.subscribers.add(writer)
                    while await reader.readline():
                        pass
                    break
                try:
                    res = {"id": rid, "ok": True, "result": await self.handle(method, params)}
                except Exception as e:
                    res = {"id": rid, "ok": False, "error": str(e)}
                writer.write((json.dumps(res) + "\n").encode())
                await writer.drain()
        finally:
            self.subscribers.discard(writer)
            writer.close()


SOCKET = ""


async def main():
    global SOCKET
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--socket", default=os.environ.get("DROVE_SOCKET", "/tmp/drove-mock.sock"))
    ap.add_argument("--tick", type=float, default=3.0, help="seconds between simulated state changes (0 = frozen)")
    ap.add_argument("--seed", type=int, default=1)
    ap.add_argument("--log", default=None, help="append every received request here")
    args = ap.parse_args()
    SOCKET = args.socket
    if os.path.exists(SOCKET):
        os.unlink(SOCKET)
    os.makedirs(os.path.dirname(SOCKET) or ".", exist_ok=True)
    m = Mock(args.seed, args.log)
    server = await asyncio.start_unix_server(m.client, path=SOCKET)
    print(f"mock drove listening on {SOCKET}", flush=True)
    async with server:
        while True:
            await asyncio.sleep(args.tick if args.tick > 0 else 3600)
            if args.tick > 0:
                await m.tick()


if __name__ == "__main__":
    try:
        asyncio.run(main())
    except KeyboardInterrupt:
        pass
