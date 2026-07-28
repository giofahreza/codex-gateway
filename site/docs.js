const legacyHashRoutes = {
  "feature-index": "/docs/",
  "quick-start": "/docs/quick-start/",
  configuration: "/docs/configuration/",
  dashboard: "/docs/dashboard/",
  "provider-accounts": "/docs/provider-accounts/",
  "routing-models": "/docs/routing-and-models/",
  "custom-models": "/docs/custom-models/",
  "test-api": "/docs/test-api/",
  "api-keys": "/docs/api-keys/",
  notifications: "/docs/notifications/",
  "usage-quota": "/docs/usage-and-quota/",
  deployment: "/docs/deployment/",
  troubleshooting: "/docs/troubleshooting/",
};

const article = document.querySelector(".docs-article");
const outline = document.getElementById("on-this-page");
const searchInput = document.getElementById("docs-search");
const searchResults = document.getElementById("docs-search-results");

const categoryAnchors = {
  Tutorials: "/docs/category/tutorials/",
  "How-to guides": "/docs/category/how-to-guides/",
  Reference: "/docs/category/reference/",
  Explanation: "/docs/category/explanation/",
  Operations: "/docs/category/operations/",
  Troubleshooting: "/docs/category/troubleshooting/",
  Routing: "/docs/category/routing/",
  Accounts: "/docs/category/accounts/",
  Limits: "/docs/category/limits/",
  Deployment: "/docs/category/deployment/",
  Notifications: "/docs/category/notifications/",
  Configuration: "/docs/category/configuration/",
  "API access": "/docs/category/api-access/",
};

const extraCategoriesBySlug = {
  "api-keys": ["API access", "Limits"],
  configuration: ["Configuration"],
  "custom-models": ["Routing"],
  dashboard: ["Accounts", "API access"],
  deployment: ["Deployment"],
  notifications: ["Notifications"],
  "priority-routing": ["Routing", "Accounts"],
  "provider-accounts": ["Accounts"],
  "quick-start": ["Configuration"],
  "routing-and-models": ["Routing"],
  "test-api": ["API access"],
  troubleshooting: ["Configuration"],
  "usage-and-quota": ["Limits", "Accounts"],
};

function redirectLegacyHashRoute() {
  const hash = decodeURIComponent(window.location.hash.slice(1));
  const cleanPath = legacyHashRoutes[hash];

  if (cleanPath && window.location.pathname.replace(/index\.html$/, "") === "/docs/") {
    window.location.replace(cleanPath);
  }
}

function slugify(value) {
  return value
    .toLowerCase()
    .replace(/[^a-z0-9]+/g, "-")
    .replace(/^-+|-+$/g, "");
}

function installActivePageState() {
  const currentPath = normalizePath(window.location.pathname);

  document.querySelectorAll(".docs-toc a[href]").forEach((link) => {
    const linkPath = normalizePath(new URL(link.href, window.location.origin).pathname);
    const isActive = linkPath === currentPath;
    link.classList.toggle("is-active", isActive);

    if (isActive) {
      link.setAttribute("aria-current", "page");
    } else {
      link.removeAttribute("aria-current");
    }
  });
}

function normalizePath(pathname) {
  const clean = pathname.replace(/index\.html$/, "");
  return clean.endsWith("/") ? clean : clean + "/";
}

function buildOutline() {
  if (!article || !outline) {
    return;
  }

  const headings = [...article.querySelectorAll("h2, h3")].filter(
    (heading) => !heading.closest(".docs-grid, .docs-related, .docs-categories"),
  );
  const usedIds = new Set([...document.querySelectorAll("[id]")].map((element) => element.id));
  const fragment = document.createDocumentFragment();

  headings.forEach((heading) => {
    if (!heading.id) {
      const base = slugify(heading.textContent.trim()) || "section";
      let id = base;
      let suffix = 2;

      while (usedIds.has(id)) {
        id = base + "-" + suffix;
        suffix += 1;
      }

      heading.id = id;
      usedIds.add(id);
    }

    const link = document.createElement("a");
    link.href = "#" + heading.id;
    link.textContent = heading.textContent.trim();
    link.dataset.depth = heading.tagName === "H3" ? "3" : "2";
    fragment.appendChild(link);
  });

  if (fragment.childNodes.length === 0) {
    outline.parentElement.hidden = true;
    return;
  }

  outline.replaceChildren(fragment);
}

function installBreadcrumbs() {
  if (!article || article.querySelector(".docs-breadcrumbs")) {
    return;
  }

  const title = article.querySelector("h1")?.textContent.trim() || "Documentation";
  const category = article.querySelector(".eyebrow")?.textContent.trim();
  const slug = article.dataset.pageSlug || "";
  const breadcrumbs = document.createElement("nav");
  breadcrumbs.className = "docs-breadcrumbs";
  breadcrumbs.setAttribute("aria-label", "Breadcrumb");

  const home = document.createElement("a");
  home.href = "/";
  home.textContent = "IO Gateway";

  const docs = document.createElement("a");
  docs.href = "/docs/";
  docs.textContent = "Docs";

  breadcrumbs.append(home, divider(), docs);

  if (slug && !slug.startsWith("category-") && category) {
    const categoryLink = document.createElement("a");
    categoryLink.href = categoryAnchors[category] || "/docs/";
    categoryLink.textContent = category;
    breadcrumbs.append(divider(), categoryLink);
  }

  const current = document.createElement("span");
  current.setAttribute("aria-current", "page");
  current.textContent = slug ? title : "Documentation";
  breadcrumbs.append(divider(), current);
  article.prepend(breadcrumbs);
}

function divider() {
  const slash = document.createElement("span");
  slash.setAttribute("aria-hidden", "true");
  slash.textContent = "/";
  return slash;
}

function installMobileOutline() {
  if (!article || !outline || article.querySelector(".docs-mobile-outline")) {
    return;
  }

  const links = [...outline.querySelectorAll("a")];

  if (!links.length) {
    return;
  }

  const details = document.createElement("details");
  details.className = "docs-mobile-outline";
  details.open = true;

  const summary = document.createElement("summary");
  summary.textContent = "Contents";

  const nav = document.createElement("nav");
  links.forEach((link) => {
    const clone = link.cloneNode(true);
    nav.appendChild(clone);
  });

  details.append(summary, nav);

  const lead = article.querySelector(".docs-lead");
  if (lead) {
    lead.after(details);
  } else {
    article.querySelector(".docs-heading")?.after(details);
  }
}

function installCategories() {
  if (!article || article.querySelector(".docs-categories")) {
    return;
  }

  const slug = article.dataset.pageSlug || "";
  if (!slug || slug.startsWith("category-")) {
    return;
  }

  const currentPageLink = document.querySelector('.docs-toc a[aria-current="page"]');
  const baseCategory = currentPageLink?.dataset.category || article.querySelector(".eyebrow")?.textContent.trim();
  const labels = [...new Set([baseCategory, ...(extraCategoriesBySlug[slug] || [])].filter(Boolean))];

  if (!labels.length) {
    return;
  }

  const categories = document.createElement("nav");
  categories.className = "docs-categories";
  categories.setAttribute("aria-label", "Page categories");

  const label = document.createElement("span");
  label.textContent = "Categories:";
  categories.appendChild(label);

  labels.forEach((name) => {
    const link = document.createElement("a");
    link.href = categoryAnchors[name] || "/docs/";
    link.textContent = name;
    categories.appendChild(link);
  });

  article.appendChild(categories);
}

function installHeadingSpy() {
  const links = [...document.querySelectorAll(".docs-on-page a")];

  if (!links.length) {
    return;
  }

  const linksById = new Map(links.map((link) => [decodeURIComponent(link.hash.slice(1)), link]));

  if (!("IntersectionObserver" in window)) {
    links[0].classList.add("is-active");
    return;
  }

  const observer = new IntersectionObserver(
    (entries) => {
      const visible = entries
        .filter((entry) => entry.isIntersecting)
        .sort((left, right) => left.boundingClientRect.top - right.boundingClientRect.top);
      const active = visible[0]?.target?.id;

      if (!active) {
        return;
      }

      links.forEach((link) => link.classList.toggle("is-active", link === linksById.get(active)));
    },
    {
      rootMargin: "-84px 0px -72% 0px",
      threshold: 0.01,
    },
  );

  linksById.forEach((_link, id) => {
    const heading = document.getElementById(id);
    if (heading) {
      observer.observe(heading);
    }
  });
}

function installSearch() {
  if (!searchInput || !searchResults) {
    return;
  }

  let pageLinks = buildFallbackSearchPages();

  fetch("/docs/search-index.json", { cache: "no-store" })
    .then((response) => (response.ok ? response.json() : Promise.reject(new Error("search index unavailable"))))
    .then((index) => {
      if (Array.isArray(index.pages) && index.pages.length) {
        pageLinks = index.pages.map((page) => ({
          title: page.title || "",
          category: page.category || "",
          summary: page.summary || "",
          keywords: page.keywords || "",
          headings: Array.isArray(page.headings) ? page.headings.join(" ") : "",
          excerpt: page.excerpt || "",
          href: page.href || "/docs/",
        }));
      }
    })
    .catch(() => {
      pageLinks = buildFallbackSearchPages();
    });

  searchInput.addEventListener("input", () => {
    const query = searchInput.value.trim().toLowerCase();

    if (!query) {
      searchResults.classList.remove("is-visible");
      searchResults.replaceChildren();
      return;
    }

    const matches = pageLinks
      .map((page) => {
        const title = page.title.toLowerCase();
        const category = page.category.toLowerCase();
        const summary = page.summary.toLowerCase();
        const keywords = page.keywords.toLowerCase();
        const headings = (page.headings || "").toLowerCase();
        const excerpt = (page.excerpt || "").toLowerCase();
        const haystack = [title, category, summary, keywords, headings, excerpt].join(" ");
        let score = 0;

        if (title.includes(query)) score += 6;
        if (category.includes(query)) score += 3;
        if (headings.includes(query)) score += 3;
        if (keywords.includes(query)) score += 2;
        if (summary.includes(query)) score += 2;
        if (excerpt.includes(query)) score += 1;
        if (!haystack.includes(query)) score = 0;

        return { ...page, score };
      })
      .filter((page) => page.score > 0)
      .sort((left, right) => right.score - left.score || left.title.localeCompare(right.title))
      .slice(0, 10);

    searchResults.replaceChildren();
    searchResults.classList.add("is-visible");

    if (matches.length === 0) {
      const empty = document.createElement("p");
      empty.className = "docs-search-empty";
      empty.textContent = "No docs pages match that search.";
      searchResults.appendChild(empty);
      return;
    }

    matches.forEach((page) => {
      const link = document.createElement("a");
      link.href = page.href;

      const title = document.createElement("strong");
      title.textContent = page.title;

      const summary = document.createElement("small");
      summary.textContent = [page.category, page.summary].filter(Boolean).join(" - ");

      link.append(title, summary);
      searchResults.appendChild(link);
    });
  });
}

function buildFallbackSearchPages() {
  return [...document.querySelectorAll(".docs-toc a[data-title]")].map((link) => ({
    title: link.dataset.title || link.textContent.trim(),
    category: link.dataset.category || "",
    summary: link.dataset.summary || "",
    keywords: link.dataset.keywords || "",
    headings: "",
    excerpt: "",
    href: link.href,
  }));
}

redirectLegacyHashRoute();
installActivePageState();
installBreadcrumbs();
buildOutline();
installMobileOutline();
installHeadingSpy();
installCategories();
installSearch();
