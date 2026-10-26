/* Verifies the wizard makes no third-party network requests.
 *
 * Why this exists: the setup wizard runs during class on school networks, and it used to
 * pull Geist from fonts.googleapis.com on every launch. A CDN font is a needless external
 * dependency for a first-run screen — it leaks that the app was opened, breaks offline, and
 * can hang the window on a filtered network. The font is bundled now, so this test fails
 * loudly if a remote font reference is ever reintroduced.
 *
 * Runs the real ui/index.html and records every outbound request.
 *
 * Usage: node tests/no-external-requests.test.mjs
 */

import { chromium } from "playwright";
import path from "path";
import fs from "fs";

const ROOT = path.resolve(import.meta.dirname, "..");
const PAGE = "file://" + path.join(ROOT, "ui", "index.html");

let pass = 0;
let fail = 0;
const check = (name, ok, extra) => {
  if (ok) {
    pass++;
    console.log("  \u2713 " + name);
  } else {
    fail++;
    console.log("  \u2717 " + name + (extra ? " \u2014 " + extra : ""));
  }
};

const CHROME = process.env.CHROME_PATH || undefined;

const run = async () => {
  const browser = await chromium.launch({
    args: ["--allow-file-access-from-files"],
    ...(CHROME ? { executablePath: CHROME } : {}),
  });
  const page = await browser.newPage({ viewport: { width: 1000, height: 680 } });

  const external = [];
  page.on("request", (req) => {
    const url = req.url();
    if (!url.startsWith("file://") && !url.startsWith("data:")) external.push(url);
  });

  await page.goto(PAGE);
  await page.waitForTimeout(1200); // give any lazy font fetch a chance to fire

  check("no external requests at all", external.length === 0, external.join(", "));

  // The bundled font must actually load, or we traded a CDN for a silent fallback.
  const geistLoaded = await page.evaluate(() =>
    document.fonts.check("400 16px 'Geist'")
  );
  check("bundled Geist loads", geistLoaded);

  const monoLoaded = await page.evaluate(() =>
    document.fonts.check("400 16px 'Geist Mono'")
  );
  check("bundled Geist Mono loads", monoLoaded);

  // And the markup must not reference the old CDN.
  const html = fs.readFileSync(path.join(ROOT, "ui", "index.html"), "utf8");
  check(
    "index.html has no Google Fonts link",
    !html.includes("fonts.googleapis.com") && !html.includes("fonts.gstatic.com")
  );

  await browser.close();
  console.log(`\n${pass} passed, ${fail} failed`);
  process.exit(fail === 0 ? 0 : 1);
};

run().catch((e) => {
  console.error(e);
  process.exit(1);
});