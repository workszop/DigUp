// The setup window's contract, checked headless: shell/ui/setup.html with a stand-in for Tauri. What Choose Folders…
// saves (finish_setup) is what's checked, folders and code folders, and that code folders alone are a setup.
//   node shell/tests/setup-contract.mjs
// Playwright comes from node's own resolution unless PLAYWRIGHT_PATH names its index.mjs; the browser is Playwright's
// own unless CHROME names one.
import { fileURLToPath, pathToFileURL } from "node:url";
import { resolve, dirname } from "node:path";

const { chromium } = await import(process.env.PLAYWRIGHT_PATH ?? "playwright");
const setupPage = pathToFileURL(resolve(dirname(fileURLToPath(import.meta.url)), "../ui/setup.html")).href;
const browser = await chromium.launch(process.env.CHROME ? { executablePath: process.env.CHROME } : {});
const failures = [];
const check = (what, actual, expected) => {
  const ok = JSON.stringify(actual) === JSON.stringify(expected);
  console.log(`${ok ? "ok  " : "FAIL"} ${what}: ${JSON.stringify(actual)}${ok ? "" : ` (expected ${JSON.stringify(expected)})`}`);
  if (!ok) failures.push(what);
};

/// The window as Choose Folders… opens it, with these folders chosen before.
async function open(chosen, codeChosen) {
  const page = await browser.newPage();
  await page.addInitScript(([chosen, codeChosen]) => {
    window.__calls = [];
    window.__TAURI__ = {
      core: {
        invoke: async (command, args) => {
          window.__calls.push({ command, args });
          if (command === "setup_info") {
            return { offered: ["/t/Documents", "/t/Downloads"], chosen, codeChosen, hotkey: "Ctrl+Shift+Space",
                     model: { state: "ready" }, auto: false };
          }
          if (command === "estimate") {
            return args.folders.map((folder) => args.code
              ? { folder, readable: true, files: { code: 12 }, bytes: 43680 }
              : { folder, readable: true, files: { pdf: 3 } });
          }
          if (command === "pick_folders") return args.code ? ["/t/work"] : ["/t/Music"];
          return null;
        },
      },
      event: { listen: async () => () => {} },
    };
  }, [chosen, codeChosen]);
  const errors = [];
  page.on("pageerror", (error) => errors.push(String(error)));
  await page.goto(setupPage);
  await page.waitForFunction(() => window.__calls.some((call) => call.command === "setup_info"));
  return { page, errors };
}

const calls = (page, command) => page.evaluate((name) => window.__calls.filter((call) => call.command === name)
  .map((call) => call.args), command);
const shownPage = (page) => page.evaluate(() => [...document.querySelectorAll(".page")].find((node) => !node.hidden)?.id);
const rows = (page, list) => page.$$eval(`#${list} .folder`, (rows) => rows.map((row) =>
  [row.querySelector(".path").textContent, row.querySelector("input").checked, row.querySelector(".counts").textContent]));

// Folders and code folders chosen before: straight to the folders, each list with its own estimates.
{
  const { page, errors } = await open(["/t/Documents"], ["/t/code"]);
  check("opens on the folders", await shownPage(page), "folders");
  await page.waitForFunction(() => document.querySelector("#code-list .counts")?.textContent !== "Counting…");
  check("code folders are estimated as code", (await calls(page, "estimate")).map((args) => [args.folders, args.code]),
        [[["/t/Documents", "/t/Downloads"], false], [["/t/code"], true]]);
  check("code rows", await rows(page, "code-list"), [["/t/code", true, "12 code files · about 44 KB of index"]]);
  await page.click("#add-code-folder");
  await page.waitForFunction(() => document.querySelectorAll("#code-list .folder").length === 2);
  check("Add Code Folder asks for code folders", (await calls(page, "pick_folders")).pop(), { code: true });
  // The first code folder out, the one added in: then through to Start.
  await page.click("#code-list .folder:first-child input");
  while ((await page.textContent("#next")) !== "Start") await page.click("#next");
  await page.click("#next");
  check("finish_setup gets folders and code folders", (await calls(page, "finish_setup")).pop(),
        { folders: ["/t/Documents"], codeFolders: ["/t/work"] });
  check("no page errors", errors, []);
}

// Code folders alone: a setup (not a first run), and enough to go on with.
{
  const { page, errors } = await open([], ["/t/code"]);
  check("code folders alone open on the folders", await shownPage(page), "folders");
  check("the usual folders start unchecked", (await rows(page, "folder-list")).map((row) => row[1]), [false, false]);
  check("Continue works with code folders alone", await page.isEnabled("#next"), true);
  await page.click("#code-list .folder input");
  check("nothing checked: Continue waits", await page.isEnabled("#next"), false);
  check("no page errors", errors, []);
}

// A first run: the welcome page, the usual folders checked but Downloads, no code folders.
{
  const { page, errors } = await open([], []);
  check("a first run opens on the welcome", await shownPage(page), "welcome");
  check("first run's folders", (await rows(page, "folder-list")).map((row) => row[1]), [true, false]);
  check("no code folders yet", (await rows(page, "code-list")).length, 0);
  while ((await page.textContent("#next")) !== "Start") await page.click("#next");
  await page.click("#next");
  check("a first run saves no code folders", (await calls(page, "finish_setup")).pop(),
        { folders: ["/t/Documents"], codeFolders: [] });
  check("no page errors", errors, []);
}

await browser.close();
if (failures.length) { console.error(`${failures.length} contract check(s) failed`); process.exit(1); }
console.log("setup contract holds");
