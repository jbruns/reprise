"""Keystroke-to-render time of the Audit screen's Entry filter over synthetic Entries, in headless Chromium.

Not a CI gate. Needs a Service serving the workbench (see README, "Workbench") and Playwright for Python:

    python3 -m venv .venv && .venv/bin/pip install playwright && .venv/bin/playwright install chromium
    .venv/bin/python crates/ui/bench/filter.py [--url http://127.0.0.1:8080] [--entries 100000] [--rounds 5]

The workbench's `/bench` screen renders the Audit screen's Entries panel over deterministic synthetic Entries; it
reads nothing from the Service. Each sample starts at the keydown's own timestamp and ends after the first frame
painted once the shown count reflects the new text and every rendered row is new and matches it.
"""

import argparse
import statistics

from playwright.sync_api import sync_playwright

# Typed one key at a time, then erased one key at a time; each key is one sample.
QUERIES = ["river raid", "pit", "usa", "00042", "zzz"]

SETUP = """
() => {
  window.__samples = [];
  // Rows rendered before the keystroke are marked, so a sample can't end on rows that merely still match.
  document.querySelector('.filter-text').addEventListener('keydown', (e) => {
    window.__down = e.timeStamp;
    window.__before = document.querySelector('p.shown').dataset.shown;
    document.querySelectorAll('tbody tr[id]').forEach((r) => { r.dataset.stale = ''; });
  }, { capture: true });
}
"""

# Resolves with the keystroke-to-paint time once the table shows `query`'s result.
SETTLED = """
(query) => new Promise((resolve) => {
  const q = query.toLowerCase();
  const done = () => {
    const shown = document.querySelector('p.shown');
    if (shown.dataset.query !== query) return false;
    // One key more or less narrows or widens the result, so the same count is the same Entries: nothing to redraw.
    if (shown.dataset.shown === window.__before) return true;
    const body = document.querySelector('#panel-entries tbody, tbody');
    if (body.querySelector('tr.loading, tr[data-stale]')) return false;
    const rows = [...body.querySelectorAll('tr[id]')];
    if (Number(shown.dataset.shown) > 0 && rows.length === 0) return false;
    return rows.every((r) => r.textContent.toLowerCase().includes(q));
  };
  const poll = () => {
    if (!done()) { requestAnimationFrame(poll); return; }
    // The frame after the one that rendered the result has been painted.
    requestAnimationFrame(() => setTimeout(() => resolve(performance.now() - window.__down), 0));
  };
  poll();
})
"""


def percentile(samples, p):
    ordered = sorted(samples)
    return ordered[min(len(ordered) - 1, round(p / 100 * (len(ordered) - 1)))]


def main():
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--url", default="http://127.0.0.1:8080")
    parser.add_argument("--entries", type=int, default=100_000)
    parser.add_argument("--rounds", type=int, default=5)
    args = parser.parse_args()

    with sync_playwright() as p:
        browser = p.chromium.launch()
        page = browser.new_page(viewport={"width": 1440, "height": 1000})
        page.goto(f"{args.url}/bench?entries={args.entries}")
        page.wait_for_selector("tbody tr[id]", timeout=120_000)
        print(f"{page.text_content('p.shown')} loaded; {browser.version}")
        page.evaluate(SETUP)
        field = page.locator(".filter-text")
        field.focus()
        samples = []
        for round_ in range(args.rounds):
            for query in QUERIES:
                for i in range(1, len(query) + 1):
                    page.keyboard.press(query[i - 1])
                    sample = page.evaluate(SETTLED, query[:i])
                    if round_ > 0:
                        samples.append(sample)
                for i in range(len(query) - 1, -1, -1):
                    page.keyboard.press("Backspace")
                    sample = page.evaluate(SETTLED, query[:i])
                    if round_ > 0:
                        samples.append(sample)
        browser.close()

    print(f"{len(samples)} keystrokes after one warm-up round over {args.entries} Entries")
    print(f"p50 {statistics.median(samples):.1f} ms, p95 {percentile(samples, 95):.1f} ms, max {max(samples):.1f} ms")


if __name__ == "__main__":
    main()
