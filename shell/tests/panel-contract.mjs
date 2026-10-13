// The search panel's DOM contract, checked headless: shell/ui/index.html with a stand-in for Tauri, fixed results,
// the keys a person presses. Breaking the contract fails this even when the panel still looks right. It lives outside
// shell/ui, so it isn't built into the app.
//   node shell/tests/panel-contract.mjs
// Playwright comes from node's own resolution unless PLAYWRIGHT_PATH names its index.mjs; the browser is Playwright's
// own unless CHROME names one.
import { fileURLToPath, pathToFileURL } from "node:url";
import { resolve, dirname } from "node:path";

const { chromium } = await import(process.env.PLAYWRIGHT_PATH ?? "playwright");
const page_ = pathToFileURL(resolve(dirname(fileURLToPath(import.meta.url)), "../ui/index.html")).href;
const HITS = [
  { path: "/t/video/lecture.mp4", kind: "video", segment: "audio", segment_id: 1, score: 0.9, modified: 1760000000, size: 1000, info: {}, loc: 6.0 },
  { path: "/t/docs/lease agreement.pdf", kind: "pdf", segment: "page", segment_id: 2, score: 0.8, modified: 1760000000, size: 2000, info: { pages: 4 }, loc: 3 },
  { path: "/t/code/src/main.rs", kind: "code", segment: "lines", segment_id: 3, score: 0.4, modified: 1760000000, size: 300, info: { lines: 40 }, loc: 10, loc_end: 20, focus: 12 },
];

const browser = await chromium.launch(process.env.CHROME ? { executablePath: process.env.CHROME } : {});
const page = await browser.newPage();
await page.addInitScript((hits) => {
  const listeners = {};
  window.__calls = [];
  window.__TAURI__ = {
    core: {
      invoke: async (command, args) => {
        window.__calls.push({ command, args });
        if (command !== "ask") return null;
        const request = args.request;
        if (request.op === "search" && request.q === "boom") return Promise.reject("search failed");
        if (request.op === "search" && request.q === "slow") return new Promise(() => {}); // an answer that never comes
        if (request.op === "search") return { hits, strong: 2, meaning: request.meaning };
        if (request.op === "render") return Promise.reject("no pictures here");
        return {};
      },
      convertFileSrc: (path) => "file://" + path,
    },
    event: { listen: async (name, handler) => { listeners[name] = handler; return () => {}; } },
    path: { homeDir: async () => "/t" },
  };
  window.__emit = (name, payload) => listeners[name]?.({ payload });
}, HITS);
const errors = [];
page.on("pageerror", (error) => errors.push(String(error)));
await page.goto(page_);

const body = (name) => page.evaluate((key) => document.body.dataset[key], name);
const failures = [];
const check = (what, actual, expected) => {
  const ok = JSON.stringify(actual) === JSON.stringify(expected);
  console.log(`${ok ? "ok  " : "FAIL"} ${what}: ${JSON.stringify(actual)}${ok ? "" : ` (expected ${JSON.stringify(expected)})`}`);
  if (!ok) failures.push(what);
};

check("starts empty", await body("phase"), "empty");
await page.fill("#query", "volcano");
await page.waitForFunction(() => document.body.dataset.phase === "meaning");
check("meaning results", await body("count"), "3");
check("top hit selected", await body("selected"), "/t/video/lecture.mp4");
check("rows carry their kind", await page.$$eval(".row", (rows) => rows.map((row) => row.dataset.kind)), ["video", "pdf"]);
check("row carries its moment", await page.$eval(".row", (row) => [row.dataset.segment, row.dataset.loc]), ["audio", "6"]);
await page.keyboard.press("ArrowDown");
check("arrow moves selection", await body("selected"), "/t/docs/lease agreement.pdf");
await page.keyboard.press("Enter");
const opened = await page.evaluate(() => window.__calls.filter((call) => call.command === "open_hit").map((call) => call.args.target));
check("enter opens at the page", opened, [{ path: "/t/docs/lease agreement.pdf", kind: "pdf", segment: "page", loc: 3, focus: null }]);
await page.fill("#query", "boom");
await page.waitForFunction(() => document.body.dataset.error !== "");
check("error is published", await body("error"), "search failed");
await page.fill("#query", "slow");
check("typing again clears the error before an answer comes", await body("error"), "");
await page.fill("#query", "");
await page.waitForFunction(() => document.body.dataset.phase === "empty");
check("cleared field is empty again", await body("count"), "0");
check("no page errors", errors, []);

await browser.close();
if (failures.length) { console.error(`${failures.length} contract check(s) failed`); process.exit(1); }
console.log("panel contract holds");
