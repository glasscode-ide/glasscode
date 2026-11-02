// The page as an agent reads it: what can be clicked or typed into, each with a number
// (data-hb-ref) the click/type tools use, and the visible text. Run in the tab.
(() => {
  const visible = (el) => {
    if (!el.getClientRects().length) return false;
    const style = getComputedStyle(el);
    return style.visibility !== "hidden" && style.display !== "none" && style.opacity !== "0";
  };
  const clip = (s, n) => {
    s = (s || "").replace(/\s+/g, " ").trim();
    return s.length > n ? s.slice(0, n - 1) + "…" : s;
  };
  const label = (el) => {
    const aria = el.getAttribute("aria-label") || el.getAttribute("title");
    if (aria) return aria;
    if (el.labels && el.labels.length) return el.labels[0].innerText;
    if (el.tagName === "INPUT" || el.tagName === "TEXTAREA") return el.getAttribute("placeholder") || el.name || "";
    return el.innerText || el.value || el.getAttribute("alt") || "";
  };
  const selector = [
    "a[href]", "button", "input:not([type=hidden])", "textarea", "select", "summary",
    "[role=button]", "[role=link]", "[role=tab]", "[role=menuitem]", "[role=option]",
    "[role=checkbox]", "[role=switch]", "[role=radio]", "[role=combobox]", "[contenteditable=true]",
  ].join(",");
  let next = Number(document.documentElement.dataset.hbNext || 1);
  const lines = [];
  for (const el of document.querySelectorAll(selector)) {
    if (!visible(el) || el.disabled) continue;
    if (!el.dataset.hbRef) el.dataset.hbRef = String(next++);
    const role = el.getAttribute("role") || el.tagName.toLowerCase();
    const kind = el.tagName === "INPUT" ? `input[${el.type}]` : role;
    let line = `[${el.dataset.hbRef}] ${kind} "${clip(label(el), 80)}"`;
    if (el.tagName === "INPUT" || el.tagName === "TEXTAREA") {
      if (el.type === "checkbox" || el.type === "radio") line += el.checked ? " checked" : " unchecked";
      else if (el.type !== "password" && el.value) line += ` value="${clip(el.value, 60)}"`;
    }
    if (el.tagName === "SELECT") line += ` value="${clip(el.value, 40)}" options=${JSON.stringify([...el.options].slice(0, 12).map((o) => o.value))}`;
    if (el.tagName === "A") line += ` → ${clip(el.getAttribute("href"), 80)}`;
    lines.push(line);
    if (lines.length >= 250) break;
  }
  document.documentElement.dataset.hbNext = String(next);
  return {
    url: location.href,
    title: document.title,
    elements: lines,
    text: clip(document.body ? document.body.innerText : "", 6000),
  };
})()
