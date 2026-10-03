const MIRROR_SELECTOR = "[data-mirror]";
const COPY_SELECTOR = "[data-copy]";
const VIEW_SELECTOR = "[data-view]";
const NAV_SELECTOR = "[data-nav]";
const TAB_SELECTOR = '[role="tab"]';

const RELEASE_PATH = "/latest";
const ASSET_PATH = "/dl";
const RELEASES_URL = "https://github.com/guuzaa/oven/releases";
const HOME_ROUTE = "/";
const DOWNLOADS_ROUTE = "/downloads";

const SITE_TITLE = "oven — a toy coding agent for joy only";
const ROUTE_TITLES = { home: SITE_TITLE, downloads: "Downloads — oven" };

const COPY_LABEL_RESET_MS = 1_200;
const SIZE_UNITS = ["B", "KiB", "MiB", "GiB"];

function el(tag, className, text) {
  const node = document.createElement(tag);
  if (className) {
    node.className = className;
  }
  if (text !== undefined) {
    node.textContent = text;
  }
  return node;
}

function formatSize(bytes) {
  let value = bytes;
  let unit = 0;
  while (value >= 1024 && unit < SIZE_UNITS.length - 1) {
    value /= 1024;
    unit += 1;
  }
  return `${value.toFixed(unit === 0 ? 0 : 1)} ${SIZE_UNITS[unit]}`;
}

function formatDate(iso) {
  const date = new Date(iso);
  return Number.isNaN(date.getTime()) ? "" : date.toLocaleDateString(undefined, { year: "numeric", month: "short", day: "numeric" });
}

// The snippets ship with this repo's hostname baked in, so a copy-paste from a
// local `wrangler dev` or a renamed zone still shows the host being browsed.
function applyMirror() {
  for (const node of document.querySelectorAll(MIRROR_SELECTOR)) {
    node.textContent = location.origin;
  }
}

function initTabs() {
  for (const list of document.querySelectorAll('[role="tablist"]')) {
    const tabs = [...list.querySelectorAll(TAB_SELECTOR)];
    const select = (active) => {
      for (const tab of tabs) {
        const selected = tab === active;
        tab.setAttribute("aria-selected", String(selected));
        const panel = document.getElementById(tab.getAttribute("aria-controls"));
        if (panel) {
          panel.hidden = !selected;
        }
      }
    };
    for (const tab of tabs) {
      tab.addEventListener("click", () => select(tab));
    }
  }
}

function initCopy() {
  for (const button of document.querySelectorAll(COPY_SELECTOR)) {
    const label = button.textContent ?? "Copy";
    let reset = 0;
    button.addEventListener("click", async () => {
      const code = button.parentElement?.querySelector("code");
      if (!code) {
        return;
      }
      try {
        await navigator.clipboard.writeText(code.textContent.trim());
        button.textContent = "Copied";
      } catch {
        button.textContent = "Failed";
      }
      window.clearTimeout(reset);
      reset = window.setTimeout(() => {
        button.textContent = label;
      }, COPY_LABEL_RESET_MS);
    });
  }
}

function activeView() {
  const { pathname } = new URL(location.href);
  return pathname.replace(/\/+$/, "") === DOWNLOADS_ROUTE ? "downloads" : "home";
}

function renderRoute() {
  const view = activeView();

  for (const section of document.querySelectorAll(VIEW_SELECTOR)) {
    section.hidden = section.dataset.view !== view;
  }
  for (const link of document.querySelectorAll(NAV_SELECTOR)) {
    if (link.dataset.nav === view) {
      link.setAttribute("aria-current", "page");
    } else {
      link.removeAttribute("aria-current");
    }
  }
  document.title = ROUTE_TITLES[view] ?? SITE_TITLE;

  const anchor = location.hash ? document.querySelector(location.hash) : null;
  if (anchor) {
    anchor.scrollIntoView({ behavior: "smooth", block: "start" });
  } else {
    window.scrollTo({ top: 0 });
  }

  if (view === "downloads") {
    void renderRelease();
  }
}

function initLinks() {
  document.addEventListener("click", (event) => {
    const link = event.target.closest("a[data-link]");
    if (!link || event.defaultPrevented || event.metaKey || event.ctrlKey || event.shiftKey || event.altKey) {
      return;
    }
    // Section links are bare fragments so they also work from file://; only
    // another view needs routing back to the home document first.
    const href = link.getAttribute("href") ?? "";
    const isSection = href.startsWith("#");
    if (isSection && activeView() === "home") {
      return;
    }
    const target = isSection ? new URL(`${HOME_ROUTE}${href}`, location.href) : new URL(link.href);
    if (target.origin !== location.origin) {
      return;
    }
    event.preventDefault();
    if (target.href !== location.href) {
      history.pushState(null, "", target.href);
    }
    renderRoute();
  });
  window.addEventListener("popstate", renderRoute);
}

function releaseRow(tag, asset) {
  const name = String(asset.name ?? "");
  const row = el("div", "release-row");
  row.append(el("span", "name", name));
  if (typeof asset.size === "number") {
    row.append(el("span", "size", formatSize(asset.size)));
  }
  const download = el("a", "button", "Download");
  download.href = `${ASSET_PATH}/${encodeURIComponent(tag)}/${encodeURIComponent(name)}`;
  row.append(download);
  return row;
}

async function renderRelease() {
  const host = document.getElementById("release");
  if (!host) {
    return;
  }
  host.replaceChildren(el("p", "release-empty", "loading latest release…"));

  let release;
  try {
    const response = await fetch(RELEASE_PATH, { headers: { accept: "application/json" } });
    if (!response.ok) {
      throw new Error(`release request failed with ${response.status}`);
    }
    release = await response.json();
  } catch {
    host.replaceChildren(el("p", "release-empty", "Could not load the release list."));
    appendReleasesLink(host);
    return;
  }

  const tag = String(release.tag_name ?? "");
  const head = el("p", "release-head");
  head.append(el("strong", undefined, tag || "latest"));
  const published = formatDate(String(release.published_at ?? ""));
  if (published) {
    head.append(el("span", undefined, `released ${published}`));
  }

  const assets = Array.isArray(release.assets) ? release.assets : [];
  host.replaceChildren(head);
  if (assets.length === 0) {
    host.append(el("p", "release-empty", "No assets attached to this release."));
  }
  for (const asset of assets) {
    host.append(releaseRow(tag, asset));
  }
  appendReleasesLink(host);
}

function appendReleasesLink(host) {
  const all = el("a", "release-empty", "Browse every release on GitHub →");
  all.href = RELEASES_URL;
  all.rel = "noopener";
  host.append(all);
}

applyMirror();
initTabs();
initCopy();
initLinks();
renderRoute();
