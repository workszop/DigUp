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
// What code search finds: one stretch of a file, its lines and the line the words are on.
const CODE_HITS = [
  { path: "/t/code/billing/retry.py", kind: "code", segment: "lines", segment_id: 7, score: 0.9, modified: 1760000000, size: 900, info: { lines: 63, repo: "/t/code/billing" }, loc: 8, loc_end: 29, focus: 12, z: 3.1 },
];

const browser = await chromium.launch(process.env.CHROME ? { executablePath: process.env.CHROME } : {});
const page = await browser.newPage();
await page.addInitScript(([hits, codeHits]) => {
  const listeners = {};
  window.__calls = [];
  window.__TAURI__ = {
    core: {
      invoke: async (command, args) => {
        window.__calls.push({ command, args });
        if (command === "code_search_on") return false;   // no code folders until "code-search" says otherwise
        if (command !== "ask") return null;
        const request = args.request;
        if (request.op === "search" && request.q === "boom") return Promise.reject("search failed");
        if (request.op === "search" && request.q === "slow") return new Promise(() => {}); // an answer that never comes
        if (request.op === "search" && request.code) return { hits: codeHits, strong: 1, meaning: request.meaning };
        if (request.op === "search") return { hits, strong: 2, meaning: request.meaning };
        if (request.op === "render") return Promise.reject("no pictures here");
        if (request.op === "text" && request.code) return { text: "def retry_with_backoff(call):\n    for attempt in range(5):" };
        return {};
      },
      convertFileSrc: (path) => "file://" + path,
    },
    event: { listen: async (name, handler) => { listeners[name] = handler; return () => {}; } },
    path: { homeDir: async () => "/t" },
  };
  window.__emit = (name, payload) => listeners[name]?.({ payload });
}, [HITS, CODE_HITS]);
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

// The gear opens Choose Folders: by click (without taking the caret from the field) and by Ctrl+, from anywhere.
const folderCalls = () => page.evaluate(() => window.__calls.filter((call) => call.command === "choose_folders").length);
await page.focus("#query");
await page.click("#folders");
check("gear click asks for Choose Folders", await folderCalls(), 1);
check("gear click leaves the caret in the field", await page.evaluate(() => document.activeElement.id), "query");
await page.keyboard.press("Control+,");
check("Ctrl+, asks for Choose Folders", await folderCalls(), 2);
await page.keyboard.press("Control+Shift+,");
await page.keyboard.press("Control+Alt+,");
check("Ctrl+Shift+, and Ctrl+Alt+, don't", await folderCalls(), 2);
await page.focus("#folders");
await page.keyboard.press("Enter");
check("Enter on the focused gear asks for Choose Folders", await folderCalls(), 3);
check("Enter on the gear opens no result", await page.evaluate(() => window.__calls.filter((call) => call.command === "open_hit").length), 0);
await page.focus("#query");
await page.keyboard.press("Control+.");
check("other Ctrl keys don't", await folderCalls(), 3);
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

// "code:" without code folders: how to turn code search on, and no search of code.
const codeSearches = () => page.evaluate(() => window.__calls.filter((call) => call.command === "ask"
  && call.args.request.op === "search" && call.args.request.code).length);
check("code search starts off", await body("code"), "off");
await page.fill("#query", "code:");
await page.waitForFunction(() => document.body.dataset.phase === "codeOff");
check("code: without code folders says how to add them", await page.$eval("#preview", (node) => node.textContent),
      "Code search is offDigUp can find code by what it does, in the folders you pick. Add code folders with the gear (Ctrl+,).");
await page.fill("#query", "code: where the retry loop is");
await page.waitForFunction(() => document.body.dataset.phase === "codeOff");
check("code: words without code folders search nothing", await codeSearches(), 0);
// Code folders chosen (the setup window): the same query searches code.
await page.evaluate(() => window.__emit("code-search", true));
check("code search is on", await body("code"), "on");
await page.waitForFunction(() => document.body.dataset.phase === "meaning");
check("code results", await body("count"), "1");
check("code rows", await page.$$eval(".row", (rows) => rows.map((row) => [row.dataset.kind, row.dataset.segment, row.dataset.loc])),
      [["code", "lines", "8"]]);
check("code group", await page.$$eval(".group-title", (titles) => titles.map((title) => title.textContent)), ["Code"]);
check("code badge", await page.$eval(".row .badge", (badge) => badge.textContent), "L8–29");
check("code preview is the text", await page.$eval("#preview .passage.code", (node) => node.textContent.split("\n")[0]),
      "def retry_with_backoff(call):");
check("code search asked for code", (await codeSearches()) > 0, true);
await page.keyboard.press("Enter");
const openedCode = await page.evaluate(() => window.__calls.filter((call) => call.command === "open_hit").map((call) => call.args.target).pop());
check("enter opens code at its line", openedCode,
      { path: "/t/code/billing/retry.py", kind: "code", segment: "lines", loc: 8, focus: 12 });
await page.fill("#query", "code:");
await page.waitForFunction(() => document.body.dataset.phase === "codeHint");
check("code: alone says what to type", await page.$eval("#preview strong", (node) => node.textContent), "Search your code");
check("no page errors", errors, []);

await browser.close();
if (failures.length) { console.error(`${failures.length} contract check(s) failed`); process.exit(1); }
console.log("panel contract holds");
