
// State pages (#311): copy buttons, and one open filter menu at a time. The pages work
// without it: every command is shown as text, and every link is a plain link.
(function () {
  function done(button) {
    button.classList.add("copied");
    setTimeout(() => button.classList.remove("copied"), 1200);
  }
  async function copy(button, text) {
    try { await navigator.clipboard.writeText(text); done(button); } catch (_) { /* no clipboard: the text is on the page */ }
  }
  for (const b of document.querySelectorAll("button[data-copy]")) {
    b.addEventListener("click", () => copy(b, b.getAttribute("data-copy")));
  }
  // "Copy as JSON": the API's view model of what the page shows (GET only).
  for (const b of document.querySelectorAll("button[data-copy-url]")) {
    b.addEventListener("click", async () => {
      try {
        const r = await fetch(new URL(b.getAttribute("data-copy-url"), location.href));
        await copy(b, JSON.stringify(await r.json(), null, 2));
      } catch (_) { /* the server stopped */ }
    });
  }
  const menus = document.querySelectorAll("details.st-facet");
  for (const m of menus) {
    m.addEventListener("toggle", () => {
      if (m.open) for (const o of menus) if (o !== m) o.open = false;
    });
  }
})();
