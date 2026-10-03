const MIRROR_SELECTOR = "[data-mirror]";
const COPY_SELECTOR = "[data-copy]";
const VIEW_SELECTOR = "[data-view]";
const NAV_SELECTOR = "[data-nav]";
const TAB_SELECTOR = '[role="tab"]';

const RELEASE_PATH = "/latest";
const ASSET_PATH = "/dl";
const RELEASES_URL = "https://github.com/guuzaa/oven/releases";
const RELEASE_TAG_URL = `${RELEASES_URL}/tag`;
const CHANGELOG_PATH = "/changelog";
const CHANGELOG_UPSTREAM = "https://raw.githubusercontent.com/guuzaa/oven/master/CHANGELOG.md";
const HTML_TYPE = "text/html";
const FILE_PROTOCOL = "file:";
const HOME_ROUTE = "/";
const HOME_VIEW = "home";
const ROUTE_VIEWS = { "/downloads": "downloads", "/releases": "releases" };

const SITE_TITLE = "oven — a toy coding agent for joy only";
const ROUTE_TITLES = { home: SITE_TITLE, downloads: "Downloads — oven", releases: "Releases — oven" };

const RELEASE_HEADING = /^## \[([^\]]+)\](?:\s+-\s+(.+))?$/;
const SECTION_HEADING = /^### (.+)$/;
const LIST_ITEM = /^- (.+)$/;
const TAGGED_VERSION = /^\d/;
const INLINE_TOKEN = /`([^`]+)`|\*\*(.+?)\*\*|\*([^*\s][^*]*)\*/g;

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

function viewFor(pathname) {
  return ROUTE_VIEWS[pathname.replace(/\/+$/, "")] ?? HOME_VIEW;
}

function shownView() {
  return document.querySelector(`${VIEW_SELECTOR}:not([hidden])`)?.dataset.view;
}

function renderRoute() {
  showView(viewFor(location.pathname), location.hash);
}

function showView(view, hash) {
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

  const anchor = hash ? document.querySelector(hash) : null;
  if (anchor) {
    anchor.scrollIntoView({ behavior: "smooth", block: "start" });
  } else {
    window.scrollTo({ top: 0 });
  }

  if (view === "downloads") {
    void renderRelease();
  } else if (view === "releases") {
    void renderChangelog();
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
    if (isSection && shownView() === HOME_VIEW) {
      return;
    }
    const target = isSection ? new URL(`${HOME_ROUTE}${href}`, location.href) : new URL(link.href);
    // No server answers /releases from file://, so views switch in place.
    if (location.protocol === FILE_PROTOCOL) {
      event.preventDefault();
      showView(viewFor(target.pathname), target.hash);
      return;
    }
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

// The worker mirrors the changelog; GitHub serves it with open CORS, so a
// local preview without the worker still renders. Any unknown worker path is
// answered with index.html, which must not be parsed as the changelog.
async function fetchChangelog() {
  for (const url of [CHANGELOG_PATH, CHANGELOG_UPSTREAM]) {
    try {
      const response = await fetch(url);
      if (response.ok && !response.headers.get("content-type")?.includes(HTML_TYPE)) {
        return await response.text();
      }
    } catch {
      continue;
    }
  }
  throw new Error("changelog is unavailable from the mirror and GitHub");
}

function parseChangelog(text) {
  const releases = [];
  let items = null;
  for (const raw of text.split("\n")) {
    const line = raw.trim();
    const heading = RELEASE_HEADING.exec(line);
    if (heading) {
      releases.push({ version: heading[1], date: heading[2] ?? "", sections: [] });
      items = null;
      continue;
    }
    const release = releases.at(-1);
    if (!release) {
      continue;
    }
    const section = SECTION_HEADING.exec(line);
    if (section) {
      items = [];
      release.sections.push({ title: section[1], items });
      continue;
    }
    const item = LIST_ITEM.exec(line);
    if (item) {
      if (!items) {
        items = [];
        release.sections.push({ title: "", items });
      }
      items.push(item[1]);
    } else if (line && items?.length) {
      items[items.length - 1] += ` ${line}`;
    }
  }
  return releases;
}

function inlineMarkdown(text) {
  const fragment = document.createDocumentFragment();
  let cursor = 0;
  for (const match of text.matchAll(INLINE_TOKEN)) {
    const [whole, code, strong, emphasis] = match;
    fragment.append(text.slice(cursor, match.index));
    if (code !== undefined) {
      fragment.append(el("code", undefined, code));
    } else if (strong !== undefined) {
      fragment.append(el("strong", undefined, strong));
    } else {
      fragment.append(el("em", undefined, emphasis));
    }
    cursor = match.index + whole.length;
  }
  fragment.append(text.slice(cursor));
  return fragment;
}

function changelogEntry({ version, date, sections }, open) {
  const tagged = TAGGED_VERSION.test(version);
  const entry = el("details", "entry");
  entry.open = open;

  const summary = el("summary", "entry-head");
  summary.append(el("span", tagged ? "entry-version" : "entry-version entry-unreleased", tagged ? `v${version}` : version));
  if (date) {
    const time = el("time", "entry-date", date);
    time.dateTime = date;
    summary.append(time);
  }
  const counts = sections
    .filter((section) => section.title)
    .map((section) => `${section.items.length} ${section.title.toLowerCase()}`)
    .join(" · ");
  if (counts) {
    summary.append(el("span", "entry-counts", counts));
  }
  entry.append(summary);

  for (const { title, items } of sections) {
    if (title) {
      entry.append(el("h3", `entry-section entry-${title.toLowerCase()}`, title));
    }
    const list = el("ul", "entry-items");
    for (const item of items) {
      const row = el("li");
      row.append(inlineMarkdown(item));
      list.append(row);
    }
    entry.append(list);
  }

  if (tagged) {
    const link = el("a", "entry-link", "View release on GitHub →");
    link.href = `${RELEASE_TAG_URL}/v${encodeURIComponent(version)}`;
    link.rel = "noopener";
    entry.append(link);
  }
  return entry;
}

async function renderChangelog() {
  const host = document.getElementById("changelog");
  if (!host) {
    return;
  }
  host.replaceChildren(el("p", "release-empty", "loading changelog…"));

  let releases;
  try {
    releases = parseChangelog(await fetchChangelog());
  } catch {
    host.replaceChildren(el("p", "release-empty", "Could not load the changelog."));
    appendReleasesLink(host);
    return;
  }

  const listed = releases.filter((release) => release.sections.some((section) => section.items.length));
  host.replaceChildren(...listed.map((release, index) => changelogEntry(release, index === 0)));
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
