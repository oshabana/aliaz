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
      '<svg class="copy-btn-icon" width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true"><rect x="9" y="9" width="11" height="11" rx="2"/><path d="M5 15V5a2 2 0 0 1 2-2h10"/></svg><span class="copy-btn-label">Copy</span>';
    return btn;
  }

  function flash(btn, label) {
    const span = btn.querySelector(".copy-btn-label");
    const prev = span.textContent;
    span.textContent = label;
    btn.classList.add("is-copied");
    clearTimeout(btn._t);
    btn._t = setTimeout(function () {
      span.textContent = prev;
      btn.classList.remove("is-copied");
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
      flash(btn, ok ? "Copied" : "Failed");
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
