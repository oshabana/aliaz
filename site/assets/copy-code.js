(function () {
  function extractCommands(pre) {
    const text = pre.innerText.replace(/ /g, " ");
    const lines = text.split("\n");
    const cmdLines = [];
    for (const raw of lines) {
      const line = raw.replace(/\s+$/, "");
      const m = line.match(/^\s*[$>]\s?(.*)$/);
      if (m) cmdLines.push(m[1]);
    }
    if (cmdLines.length > 0) return cmdLines.join("\n");
    return text.replace(/\s+$/, "");
  }

  function makeButton() {
    const btn = document.createElement("button");
    btn.type = "button";
    btn.className = "copy-btn";
    btn.setAttribute("aria-label", "Copy commands");
    btn.innerHTML =
      '<svg class="copy-btn-icon copy-btn-icon-copy" width="15" height="15" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true"><rect x="9" y="9" width="11" height="11" rx="2"/><path d="M5 15V5a2 2 0 0 1 2-2h10"/></svg><svg class="copy-btn-icon copy-btn-icon-check" width="15" height="15" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2.4" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true"><path d="M5 12.5l4.5 4.5L20 6.5"/></svg><span class="copy-btn-label" hidden>Copy</span>';
    return btn;
  }

  function flash(btn, ok) {
    btn.setAttribute(
      "aria-label",
      ok ? "Copied to clipboard" : "Copy failed",
    );
    btn.classList.toggle("is-copied", !!ok);
    btn.classList.toggle("is-failed", !ok);
    clearTimeout(btn._t);
    btn._t = setTimeout(function () {
      btn.classList.remove("is-copied");
      btn.classList.remove("is-failed");
      btn.setAttribute("aria-label", "Copy commands");
    }, 1600);
  }

  async function copyText(text) {
    if (navigator.clipboard && window.isSecureContext) {
      try {
        await navigator.clipboard.writeText(text);
        return true;
      } catch (e) {}
    }
    try {
      const ta = document.createElement("textarea");
      ta.value = text;
      ta.setAttribute("readonly", "");
      ta.style.position = "absolute";
      ta.style.left = "-9999px";
      document.body.appendChild(ta);
      ta.select();
      const ok = document.execCommand("copy");
      document.body.removeChild(ta);
      return ok;
    } catch (e) {
      return false;
    }
  }

  function attach(pre) {
    if (pre.dataset.copyAttached) return;
    if (!pre.querySelector("code")) return;
    pre.dataset.copyAttached = "1";
    pre.classList.add("has-copy-btn");
    const btn = makeButton();
    btn.addEventListener("click", async function () {
      const text = extractCommands(pre);
      const ok = await copyText(text);
      flash(btn, ok);
    });
    pre.appendChild(btn);
  }

  function init() {
    document.querySelectorAll("pre").forEach(attach);
  }

  if (document.readyState === "loading") {
    document.addEventListener("DOMContentLoaded", init);
  } else {
    init();
  }
})();
