// State pages (#311): copy buttons, and one open filter menu at a time. The pages work
// without it: every command is shown as text, and every link is a plain link.
(function () {
  const live = document.getElementById("st-live");
  function say(message) {
    if (!live) return;
    // Cleared first, so the same message is announced again.
    live.textContent = "";
    setTimeout(() => { live.textContent = message; }, 50);
  }
  function done(button) {
    button.classList.add("copied");
    setTimeout(() => button.classList.remove("copied"), 1200);
    say("Copied");
  }
  // Without clipboard access (e.g. no secure context), select the text instead.
  function fallback(source) {
    if (source) {
      const range = document.createRange();
      range.selectNodeContents(source);
      const selection = window.getSelection();
      selection.removeAllRanges();
      selection.addRange(range);
    }
    say("Selected: press Ctrl+C to copy");
  }
  async function copy(button, text, source) {
    try { await navigator.clipboard.writeText(text); done(button); } catch (_) { fallback(source); }
  }
  for (const b of document.querySelectorAll("button[data-copy]")) {
    const source = b.previousElementSibling && b.previousElementSibling.tagName === "CODE"
      ? b.previousElementSibling : null;
    b.addEventListener("click", () => copy(b, b.getAttribute("data-copy"), source));
  }
  // "Copy as JSON": the API's view model of what the page shows (GET only).
  for (const b of document.querySelectorAll("button[data-copy-url]")) {
    b.addEventListener("click", async () => {
      try {
        const r = await fetch(new URL(b.getAttribute("data-copy-url"), location.href));
        await copy(b, JSON.stringify(await r.json(), null, 2), null);
      } catch (_) { say("Couldn't copy: the server didn't answer"); }
    });
  }
  const menus = document.querySelectorAll("details.st-facet");
  for (const m of menus) {
    m.addEventListener("toggle", () => {
      if (m.open) for (const o of menus) if (o !== m) o.open = false;
    });
  }
})();
