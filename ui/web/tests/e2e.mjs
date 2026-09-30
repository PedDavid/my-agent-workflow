// Browser check: node ui/web/tests/e2e.mjs   (after `cargo build -p drove-web` in ui/)
// Starts the mock daemon + drove-web, drives the page with Playwright/Chromium.
import { spawn } from "node:child_process";
import { createRequire } from "node:module";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import assert from "node:assert/strict";
import { fileURLToPath } from "node:url";

const here = path.dirname(fileURLToPath(import.meta.url));
const require = createRequire(import.meta.url);
let chromium;
try {
  ({ chromium } = require("playwright"));
} catch {
  const root = process.env.PLAYWRIGHT_NODE_MODULES || "/opt/node22/lib/node_modules";
  ({ chromium } = require(path.join(root, "playwright")));
}

const tmp = fs.mkdtempSync(path.join(os.tmpdir(), "drove-e2e-"));
const sock = path.join(tmp, "d.sock");
const log = path.join(tmp, "req.log");
const shots = path.join(here, "..", "screenshots");
fs.mkdirSync(shots, { recursive: true });
const procs = [];
const cleanup = () => procs.forEach((p) => p.kill());
process.on("exit", cleanup);
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));

procs.push(
  spawn("python3", [path.join(here, "../../../tools/mock-drove.py"), "--socket", sock, "--tick", "0", "--log", log], {
    stdio: "ignore",
  }),
);
for (let i = 0; i < 100 && !fs.existsSync(sock); i++) await sleep(50);

const web = spawn(path.join(here, "../../target/debug/drove-web"), ["--listen", "127.0.0.1:0", "--socket", sock], {
  stdio: ["ignore", "ignore", "pipe"],
});
procs.push(web);
const base = await new Promise((res, rej) => {
  let buf = "";
  web.stderr.on("data", (d) => {
    buf += d;
    const m = buf.match(/http:\/\/[\d.:]+/);
    if (m) res(m[0]);
  });
  web.on("exit", () => rej(new Error("drove-web exited: " + buf)));
});

const reqs = () =>
  fs.existsSync(log)
    ? fs.readFileSync(log, "utf8").trim().split("\n").filter(Boolean).map((l) => JSON.parse(l))
    : [];
const count = (m) => reqs().filter((r) => r.method === m).length;
async function waitCount(m, n) {
  for (let i = 0; i < 50 && count(m) <= n; i++) await sleep(100);
  return count(m);
}

const browser = await chromium.launch({ headless: true });
try {
  for (const scheme of ["light", "dark"]) {
    const ctx = await browser.newContext({ viewport: { width: 1200, height: 800 }, colorScheme: scheme });
    const page = await ctx.newPage();
    const errors = [];
    page.on("pageerror", (e) => errors.push(e.message));
    await page.goto(base);
    await page.waitForSelector(".card");
    assert.equal(await page.locator(".card").count(), 4, "four mock agents render");
    const names = await page.locator(".card .name").allTextContents();
    assert.equal(names[0], "fix-flaky-tests", "needs_input sorts first");
    assert.match(await page.title(), /^\(\d+!\) drove$/);
    assert.ok((await page.locator(".card.attention").count()) >= 1);

    if (scheme === "light") {
      const scratch = page.locator(".card", { hasText: "scratch" });
      await scratch.getByText("Preview").click();
      await page.waitForFunction(() =>
        [...document.querySelectorAll(".card.open pre")].some((p) => p.textContent.includes("scratch")),
      );
      const before = count("focus");
      await page.locator(".card", { hasText: "api-refactor" }).locator(".name").click();
      assert.ok((await waitCount("focus", before)) > before, "clicking a card sent focus");
      const last = reqs().filter((r) => r.method === "focus").at(-1);
      assert.equal(last.params.id.length, 6);
      const nb = count("next");
      await page.getByRole("button", { name: "Next" }).click();
      assert.ok((await waitCount("next", nb)) > nb, "Next sent");
      await scratch.getByText("Preview").click();
    }
    await page.screenshot({ path: path.join(shots, `desktop-${scheme}.png`), fullPage: true });
    assert.deepEqual(errors, []);
    await ctx.close();
  }
  const mob = await browser.newContext({ viewport: { width: 390, height: 844 }, deviceScaleFactor: 2 });
  const mp = await mob.newPage();
  await mp.goto(base);
  await mp.waitForSelector(".card");
  const overflow = await mp.evaluate(() => document.documentElement.scrollWidth > window.innerWidth);
  assert.equal(overflow, false, "no horizontal scroll at 390px");
  await mp.screenshot({ path: path.join(shots, "mobile-390.png"), fullPage: true });
  await mob.close();
  console.log("e2e ok; screenshots in", shots);
} finally {
  await browser.close();
  cleanup();
}
