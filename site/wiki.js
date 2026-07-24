const pages = [...document.querySelectorAll(".wiki-page")];
const pageIds = new Set(pages.map((page) => page.id));
const wikiLinks = [...document.querySelectorAll('a[href^="#"]')].filter((link) => {
  const id = decodeURIComponent(link.getAttribute("href").slice(1));
  return pageIds.has(id);
});
const defaultPageId = pageIds.has("feature-index") ? "feature-index" : pages[0]?.id;

function getHashPageId() {
  const id = decodeURIComponent(window.location.hash.slice(1));
  return pageIds.has(id) ? id : defaultPageId;
}

function setActivePage(pageId, options = {}) {
  if (!pageIds.has(pageId)) {
    return;
  }

  pages.forEach((page) => {
    const isActive = page.id === pageId;
    page.hidden = !isActive;
    page.classList.toggle("is-active", isActive);
    page.setAttribute("aria-hidden", isActive ? "false" : "true");
  });

  wikiLinks.forEach((link) => {
    const isActive = decodeURIComponent(link.getAttribute("href").slice(1)) === pageId;
    link.classList.toggle("is-active", isActive);
    if (isActive) {
      link.setAttribute("aria-current", "page");
    } else {
      link.removeAttribute("aria-current");
    }
  });

  const activePage = document.getElementById(pageId);
  const title = activePage?.querySelector("h2")?.textContent?.trim();
  document.title = title && pageId !== defaultPageId ? `${title} - IO Gateway Wiki` : "IO Gateway Wiki";

  if (options.scroll) {
    const scrollTarget = window.matchMedia("(max-width: 720px)").matches
      ? activePage
      : document.querySelector(".wiki-layout");
    scrollTarget?.scrollIntoView({ block: "start" });
  }

  if (options.focus) {
    activePage?.focus({ preventScroll: true });
  }
}

if (defaultPageId) {
  pages.forEach((page) => {
    page.tabIndex = -1;
  });

  document.body.classList.add("wiki-js-ready");
  setActivePage(getHashPageId(), { scroll: Boolean(window.location.hash) });

  wikiLinks.forEach((link) => {
    link.addEventListener("click", (event) => {
      const pageId = decodeURIComponent(link.getAttribute("href").slice(1));
      if (!pageIds.has(pageId)) {
        return;
      }

      event.preventDefault();
      if (window.location.hash !== `#${pageId}`) {
        window.history.pushState(null, "", `#${pageId}`);
      }
      setActivePage(pageId, { focus: true, scroll: true });
    });
  });

  window.addEventListener("popstate", () => {
    setActivePage(getHashPageId(), { scroll: true });
  });
}
