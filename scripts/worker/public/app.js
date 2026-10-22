const MIRROR_SELECTOR = "[data-mirror]";
const COPY_SELECTOR = "[data-copy]";
const VIEW_SELECTOR = "[data-view]";
const NAV_SELECTOR = "[data-nav]";
const TAB_SELECTOR = '[role="tab"]';
const REVEAL_SELECTOR = "[data-reveal]";
const RIPPLE_TARGETS = ".button, .copy, .tab";

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

const ASSET_TARGET = /-([a-z0-9_]+)-[a-z]+-(darwin|linux|windows)(?:-([a-z]+))?\.(?:tar\.gz|zip)$/;
const PLATFORMS = [
  { os: "darwin", name: "macOS", icon: "🍎" },
  { os: "linux", name: "Linux", icon: "🐧" },
  { os: "windows", name: "Windows", icon: "🪟" },
  { os: "", name: "Other files", icon: "📦" },
];
const OS_HINTS = [
  ["darwin", /mac/i],
  ["windows", /win/i],
  ["linux", /linux|x11/i],
];
const ARCH_LABELS = { x86_64: "x86-64", aarch64: "ARM64", loongarch64: "LoongArch64" };
const MAC_ARCH_LABELS = { x86_64: "Intel", aarch64: "Apple Silicon" };
const LIBC_LABELS = { gnu: "glibc 2.28+", musl: "musl · static" };
const CURRENT_PLATFORM_LABEL = "your system";

const COPY_LABEL_RESET_MS = 1_200;
const SIZE_UNITS = ["B", "KiB", "MiB", "GiB"];
const REDUCED_MOTION = window.matchMedia("(prefers-reduced-motion: reduce)");
const REVEAL_STAGGER_MS = 70;
const REVEAL_STAGGER_STEPS = 6;
const SKELETON_PLATFORMS = 3;
const SKELETON_ASSETS = 2;
const SKELETON_ENTRIES = 3;
const SCROLL_OFFSET_PX = 480;
const SCROLL_BEHAVIOR = () => (REDUCED_MOTION.matches ? "auto" : "smooth");

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

function skeleton(className) {
  const node = el("span", `skeleton ${className}`);
  node.setAttribute("aria-hidden", "true");
  return node;
}

function skeletonLine(width) {
  const line = skeleton("skeleton-line");
  line.style.width = width;
  return line;
}

function releaseSkeleton() {
  const head = el("div", "release-head");
  head.append(skeleton("skeleton-badge"), skeletonLine("8rem"));

  const cards = [];
  for (let index = 0; index < SKELETON_PLATFORMS; index += 1) {
    const card = el("section", "platform");
    const cardHead = el("div", "platform-head");
    cardHead.append(skeleton("skeleton-icon"), skeletonLine("6rem"));
    card.append(cardHead);
    for (let row = 0; row < SKELETON_ASSETS; row += 1) {
      const asset = el("div", "asset");
      asset.append(skeletonLine("9.5rem"), skeletonLine("12rem"), skeleton("skeleton-button"));
      card.append(asset);
    }
    cards.push(card);
  }
  return [head, ...cards];
}

function changelogSkeleton() {
  const entries = [];
  for (let index = 0; index < SKELETON_ENTRIES; index += 1) {
    const entry = el("div", "entry skeleton-entry");
    const head = el("div", "entry-head");
    head.append(skeleton("skeleton-badge"), skeletonLine("7rem"), skeletonLine("9rem"));
    const body = el("div", "entry-body");
    body.append(skeletonLine("100%"), skeletonLine("86%"), skeletonLine("52%"));
    entry.append(head, body);
    entries.push(entry);
  }
  return entries;
}

function stagger(nodes) {
  nodes.forEach((node, index) => node.style.setProperty("--delay", `${Math.min(index, REVEAL_STAGGER_STEPS) * REVEAL_STAGGER_MS}ms`));
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
        button.classList.add("is-copied");
      } catch {
        button.textContent = "Failed";
      }
      window.clearTimeout(reset);
      reset = window.setTimeout(() => {
        button.textContent = label;
        button.classList.remove("is-copied");
      }, COPY_LABEL_RESET_MS);
    });
  }
}

const revealObserver =
  "IntersectionObserver" in window
    ? new IntersectionObserver(
        (entries, observer) => {
          for (const entry of entries) {
            if (!entry.isIntersecting) {
              continue;
            }
            entry.target.classList.add("is-in");
            observer.unobserve(entry.target);
          }
        },
        { rootMargin: "0px 0px -8% 0px" },
      )
    : null;

function initReveal() {
  const nodes = [...document.querySelectorAll(REVEAL_SELECTOR)];
  const groups = new Map();
  for (const node of nodes) {
    const siblings = groups.get(node.parentElement);
    if (siblings) {
      siblings.push(node);
    } else {
      groups.set(node.parentElement, [node]);
    }
  }
  for (const siblings of groups.values()) {
    stagger(siblings);
  }
  if (!revealObserver) {
    for (const node of nodes) {
      node.classList.add("is-in");
    }
    return;
  }
  for (const node of nodes) {
    revealObserver.observe(node);
  }
}

function initRipple() {
  document.addEventListener("pointerdown", (event) => {
    const target = event.target.closest(RIPPLE_TARGETS);
    if (!target || REDUCED_MOTION.matches || target.matches(":disabled")) {
      return;
    }
    const box = target.getBoundingClientRect();
    const size = 2 * Math.max(box.width, box.height);
    const ripple = document.createElement("span");
    ripple.className = "ripple";
    ripple.setAttribute("aria-hidden", "true");
    ripple.style.width = `${size}px`;
    ripple.style.height = `${size}px`;
    ripple.style.left = `${event.clientX - box.left - size / 2}px`;
    ripple.style.top = `${event.clientY - box.top - size / 2}px`;
    ripple.addEventListener("animationend", () => ripple.remove());
    target.append(ripple);
  });
}

function initScrollChrome() {
  const progress = document.querySelector(".scroll-progress");
  const toTop = document.querySelector(".to-top");
  let frame = 0;
  let lastRatio = -1;
  let lastScrolled = null;

  const update = () => {
    frame = 0;
    const travel = document.documentElement.scrollHeight - window.innerHeight;
    const ratio = travel > 0 ? Math.min(window.scrollY / travel, 1) : 0;
    if (ratio !== lastRatio) {
      lastRatio = ratio;
      progress?.style.setProperty("--progress", ratio.toFixed(4));
    }
    const scrolled = window.scrollY > SCROLL_OFFSET_PX;
    if (scrolled !== lastScrolled) {
      lastScrolled = scrolled;
      toTop?.classList.toggle("is-shown", scrolled);
    }
  };
  const schedule = () => {
    frame ||= window.requestAnimationFrame(update);
  };

  window.addEventListener("scroll", schedule, { passive: true });
  window.addEventListener("resize", schedule, { passive: true });
  update();
  toTop?.addEventListener("click", () => window.scrollTo({ top: 0, behavior: SCROLL_BEHAVIOR() }));
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
    anchor.scrollIntoView({ behavior: SCROLL_BEHAVIOR(), block: "start" });
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

function detectOs() {
  const platform = navigator.userAgentData?.platform || navigator.userAgent;
  return OS_HINTS.find(([, hint]) => hint.test(platform))?.[0];
}

function describeAsset(asset) {
  const name = String(asset.name ?? "");
  const [, arch = "", os = "", libc = ""] = ASSET_TARGET.exec(name) ?? [];
  const archLabels = os === "darwin" ? MAC_ARCH_LABELS : ARCH_LABELS;
  return {
    name,
    size: asset.size,
    os,
    arch: archLabels[arch] ?? arch,
    variant: os === "linux" ? (LIBC_LABELS[libc] ?? libc) : "",
  };
}

function byLabel(left, right) {
  return left.arch.localeCompare(right.arch) || left.variant.localeCompare(right.variant) || left.name.localeCompare(right.name);
}

function assetRow(tag, { name, size, arch, variant }) {
  const row = el("div", "asset");
  if (arch) {
    const label = el("div", "asset-label");
    label.append(el("strong", undefined, arch));
    if (variant) {
      label.append(el("span", "asset-variant", variant));
    }
    row.append(label);
  }
  row.append(el("span", "asset-file", name));
  if (typeof size === "number") {
    row.append(el("span", "asset-size", formatSize(size)));
  }
  const download = el("a", "button", "Download");
  download.href = `${ASSET_PATH}/${encodeURIComponent(tag)}/${encodeURIComponent(name)}`;
  row.append(download);
  return row;
}

function platformCard(tag, { name, icon }, assets, current) {
  const card = el("section", "platform");
  const head = el("h2", "platform-head");
  const badge = el("span", "platform-icon", icon);
  badge.setAttribute("aria-hidden", "true");
  head.append(badge, name);
  if (current) {
    head.append(el("span", "platform-current", CURRENT_PLATFORM_LABEL));
  }
  card.append(head, ...assets.map((asset) => assetRow(tag, asset)));
  return card;
}

async function renderRelease() {
  const host = document.getElementById("release");
  if (!host) {
    return;
  }
  host.replaceChildren(...releaseSkeleton());

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

  const assets = Array.isArray(release.assets) ? release.assets.map(describeAsset) : [];
  host.replaceChildren(head);
  if (assets.length === 0) {
    host.append(el("p", "release-empty", "No assets attached to this release."));
  }
  const current = detectOs();
  const platforms = PLATFORMS.toSorted((left, right) => Number(right.os === current) - Number(left.os === current));
  const cards = [];
  for (const platform of platforms) {
    const matching = assets.filter((asset) => asset.os === platform.os).sort(byLabel);
    if (matching.length) {
      cards.push(platformCard(tag, platform, matching, platform.os === current));
    }
  }
  stagger(cards);
  host.append(...cards);
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

  const body = el("div", "entry-body");
  for (const { title, items } of sections) {
    if (title) {
      body.append(el("h3", `entry-section entry-${title.toLowerCase()}`, title));
    }
    const list = el("ul", "entry-items");
    for (const item of items) {
      const row = el("li");
      row.append(inlineMarkdown(item));
      list.append(row);
    }
    body.append(list);
  }

  if (tagged) {
    const link = el("a", "entry-link", "View release on GitHub →");
    link.href = `${RELEASE_TAG_URL}/v${encodeURIComponent(version)}`;
    link.rel = "noopener";
    body.append(link);
  }
  entry.append(body);
  return entry;
}

async function renderChangelog() {
  const host = document.getElementById("changelog");
  if (!host) {
    return;
  }
  host.replaceChildren(...changelogSkeleton());

  let releases;
  try {
    releases = parseChangelog(await fetchChangelog());
  } catch {
    host.replaceChildren(el("p", "release-empty", "Could not load the changelog."));
    appendReleasesLink(host);
    return;
  }

  const listed = releases.filter((release) => release.sections.some((section) => section.items.length));
  const cards = listed.map((release, index) => changelogEntry(release, index === 0));
  stagger(cards);
  host.replaceChildren(...cards);
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
initReveal();
initRipple();
initScrollChrome();
renderRoute();
