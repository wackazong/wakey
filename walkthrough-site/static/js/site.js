// What the pages do beyond showing text: theme switch, chapter drawer,
// scroll progress, the table of contents following the text, copy buttons,
// chapters read, search, and Mermaid diagrams. No library but Mermaid, which
// is loaded only on a page that has a diagram. The page is complete without
// this script; everything here is an addition.
(function () {
  "use strict";

  var doc = document;
  var root = doc.documentElement;
  var body = doc.body;
  // The relative way to the site root ("../../"), and the placeholder
  // address Zola wrote into the search index (templates/base.html).
  var ROOT = body.dataset.root || "./";
  var BASE = body.dataset.base || "/";

  function store(key, value) {
    try {
      if (value === undefined) return localStorage.getItem(key);
      localStorage.setItem(key, value);
    } catch (e) {
      // No storage: nothing is remembered.
    }
    return null;
  }

  // ── Theme ───────────────────────────────────────────────────────────────

  function currentTheme() {
    return (
      root.getAttribute("data-theme") ||
      (matchMedia("(prefers-color-scheme: dark)").matches ? "dark" : "light")
    );
  }

  var themeButton = doc.getElementById("theme-toggle");
  if (themeButton) {
    themeButton.addEventListener("click", function () {
      var next = currentTheme() === "dark" ? "light" : "dark";
      root.setAttribute("data-theme", next);
      store("theme", next);
      if (window.applyCodeTheme) window.applyCodeTheme();
      drawDiagrams();
    });
  }
  matchMedia("(prefers-color-scheme: dark)").addEventListener("change", function () {
    if (!root.getAttribute("data-theme")) drawDiagrams();
  });

  // ── Chapter drawer (narrow screens) ─────────────────────────────────────

  var menuButton = doc.getElementById("menu-toggle");
  var scrim = doc.getElementById("scrim");
  function setMenu(open) {
    body.classList.toggle("menu-open", open);
    if (menuButton) menuButton.setAttribute("aria-expanded", String(open));
    if (scrim) scrim.hidden = !open;
  }
  if (menuButton) {
    menuButton.addEventListener("click", function () {
      setMenu(!body.classList.contains("menu-open"));
    });
  }
  if (scrim) {
    scrim.addEventListener("click", function () {
      setMenu(false);
    });
  }

  // ── Scroll progress and the table of contents ───────────────────────────

  var bar = doc.getElementById("scroll-progress");
  var tocLinks = Array.prototype.slice.call(doc.querySelectorAll(".toc a"));
  var headings = tocLinks
    .map(function (a) {
      return doc.getElementById(decodeURIComponent(a.hash.slice(1)));
    })
    .filter(Boolean);
  var ticking = false;

  function onScroll() {
    ticking = false;
    var max = root.scrollHeight - innerHeight;
    if (bar) bar.style.transform = "scaleX(" + (max > 0 ? Math.min(1, scrollY / max) : 0) + ")";
    if (!headings.length) return;
    // The last heading above a line a little below the top bar.
    var line = 120;
    var active = null;
    for (var i = 0; i < headings.length; i++) {
      if (headings[i].getBoundingClientRect().top <= line) active = headings[i];
    }
    tocLinks.forEach(function (a) {
      a.classList.toggle("is-active", !!active && a.hash === "#" + active.id);
    });
  }
  addEventListener(
    "scroll",
    function () {
      if (!ticking) {
        ticking = true;
        requestAnimationFrame(onScroll);
      }
    },
    { passive: true }
  );
  onScroll();

  // ── Chapters read ───────────────────────────────────────────────────────

  var chapter = doc.querySelector("main[data-chapter]");
  if (chapter) store("read:" + chapter.dataset.chapter, "1");
  doc.querySelectorAll(".sidebar__list [data-chapter]").forEach(function (li) {
    if (store("read:" + li.dataset.chapter)) li.classList.add("is-read");
  });
  var here = doc.querySelector(".sidebar__list .is-current");
  if (here && here.scrollIntoView) here.scrollIntoView({ block: "nearest" });

  // ── Copy buttons ────────────────────────────────────────────────────────

  // The code of an excerpt as in the file: no line numbers; a gap stays as
  // its `// ...` line.
  function excerptText(figure) {
    var lines = [];
    figure.querySelectorAll(".giallo-l, .code__gap").forEach(function (el) {
      if (el.classList.contains("code__gap")) {
        lines.push(el.textContent);
        return;
      }
      var clone = el.cloneNode(true);
      clone.querySelectorAll(".giallo-ln").forEach(function (n) {
        n.remove();
      });
      lines.push(clone.textContent);
    });
    return lines.join("\n") + "\n";
  }

  if (navigator.clipboard) {
    doc.querySelectorAll(".code__copy").forEach(function (button) {
      button.hidden = false;
      button.addEventListener("click", function () {
        navigator.clipboard.writeText(excerptText(button.closest(".code"))).then(function () {
          button.textContent = "Copied";
          button.classList.add("is-done");
          setTimeout(function () {
            button.textContent = "Copy";
            button.classList.remove("is-done");
          }, 1600);
        });
      });
    });
  }

  // ── Search ──────────────────────────────────────────────────────────────

  var dialog = doc.getElementById("search");
  var input = doc.getElementById("search-input");
  var results = doc.getElementById("search-results");
  var hint = doc.getElementById("search-hint");
  var index = null; // [{ url, title, body, lower }]
  var loading = null;
  var selected = -1;

  // Walkthroughs and chapters by path, to name a result.
  var places = {};
  try {
    JSON.parse(doc.getElementById("site-map").textContent).forEach(function (w) {
      places[w.path] = { walkthrough: w.title };
      w.chapters.forEach(function (c) {
        places[c.path] = { walkthrough: w.title, chapter: c.n };
      });
    });
  } catch (e) {
    // Results then show their titles only.
  }

  function pathOf(url) {
    return "/" + (url.indexOf(BASE) === 0 ? url.slice(BASE.length) : url);
  }

  // The index holds the page's text as it stands, with the labels of code
  // excerpts ("path/file.rslines 5–7Copy") and the "#" of heading links.
  function tidy(text) {
    return text.replace(/\S+lines? \d+(?:–\d+)?Copy/g, " ").replace(/(\S)#(\s)/g, "$1:$2");
  }

  function loadIndex() {
    if (!loading) {
      loading = fetch(ROOT + "search_index.en.json")
        .then(function (r) {
          return r.json();
        })
        .then(function (entries) {
          index = entries.map(function (e) {
            return {
              path: pathOf(e.url),
              // Titles may carry Markdown backticks for code.
              title: (e.title || "").replace(/`/g, ""),
              body: tidy(e.body || ""),
              lowerTitle: (e.title || "").toLowerCase(),
              lowerBody: tidy(e.body || "").toLowerCase(),
            };
          });
        })
        .catch(function () {
          hint.textContent = "The search index could not be loaded.";
        });
    }
    return loading;
  }

  function escapeHtml(s) {
    return s.replace(/[&<>"]/g, function (c) {
      return { "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;" }[c];
    });
  }

  // Every word must occur; the title counts most, then how often.
  function search(query) {
    var words = query.toLowerCase().split(/\s+/).filter(Boolean);
    if (!words.length || !index) return [];
    var hits = [];
    index.forEach(function (e) {
      var score = 0;
      var first = -1;
      for (var i = 0; i < words.length; i++) {
        var w = words[i];
        var inTitle = e.lowerTitle.indexOf(w) >= 0;
        var at = e.lowerBody.indexOf(w);
        if (!inTitle && at < 0) return;
        if (inTitle) score += 20;
        var count = 0;
        for (var p = at; p >= 0 && count < 20; p = e.lowerBody.indexOf(w, p + w.length)) count++;
        score += count;
        if (at >= 0 && (first < 0 || at < first)) first = at;
      }
      hits.push({ entry: e, score: score, at: first, words: words });
    });
    hits.sort(function (a, b) {
      return b.score - a.score;
    });
    return hits.slice(0, 12);
  }

  function snippet(hit) {
    var text = hit.entry.body;
    if (hit.at < 0) return escapeHtml(text.slice(0, 140));
    var start = Math.max(0, hit.at - 60);
    var end = Math.min(text.length, hit.at + 110);
    var cut = (start > 0 ? "… " : "") + text.slice(start, end).replace(/\s+/g, " ") + (end < text.length ? " …" : "");
    var html = escapeHtml(cut);
    hit.words.forEach(function (w) {
      var pattern = new RegExp("(" + w.replace(/[.*+?^${}()|[\]\\]/g, "\\$&") + ")", "gi");
      html = html.replace(pattern, "<mark>$1</mark>");
    });
    return html;
  }

  function where(path) {
    var place = places[path];
    if (!place) return "";
    return place.chapter ? place.walkthrough + " · Chapter " + place.chapter : place.walkthrough + " · Overview";
  }

  function select(i) {
    var items = results.children;
    if (!items.length) {
      selected = -1;
      return;
    }
    selected = (i + items.length) % items.length;
    for (var n = 0; n < items.length; n++) {
      items[n].setAttribute("aria-selected", String(n === selected));
    }
    items[selected].scrollIntoView({ block: "nearest" });
  }

  function render() {
    var query = input.value.trim();
    var hits = search(query);
    results.innerHTML = hits
      .map(function (hit) {
        var e = hit.entry;
        var place = where(e.path);
        return (
          '<li role="option"><a href="' +
          ROOT +
          escapeHtml(e.path.slice(1)) +
          '">' +
          (place ? '<span class="search__where">' + escapeHtml(place) + "</span>" : "") +
          '<span class="search__title">' +
          escapeHtml(e.title) +
          "</span>" +
          '<span class="search__snippet">' +
          snippet(hit) +
          "</span></a></li>"
        );
      })
      .join("");
    if (!query) {
      hint.innerHTML = "Type to search. <kbd>↑</kbd> <kbd>↓</kbd> to choose, <kbd>Enter</kbd> to open.";
    } else if (!hits.length && index) {
      hint.textContent = "Nothing found for “" + query + "”.";
    } else {
      hint.textContent = hits.length + (hits.length === 1 ? " result" : " results");
    }
    select(0);
  }

  function openSearch(query) {
    if (!dialog || dialog.open) return;
    setMenu(false);
    dialog.showModal();
    if (typeof query === "string") input.value = query;
    input.select();
    loadIndex().then(render);
  }

  if (dialog && dialog.showModal) {
    doc.getElementById("search-open").addEventListener("click", openSearch);
    // `?q=words` in the address opens the search with them.
    var asked = new URLSearchParams(location.search).get("q");
    if (asked) openSearch(asked);
    input.addEventListener("input", render);
    input.addEventListener("keydown", function (ev) {
      if (ev.key === "ArrowDown") {
        ev.preventDefault();
        select(selected + 1);
      } else if (ev.key === "ArrowUp") {
        ev.preventDefault();
        select(selected - 1);
      } else if (ev.key === "Enter") {
        ev.preventDefault();
        var item = results.children[selected];
        if (item) item.querySelector("a").click();
      }
    });
    // A click beside the dialog closes it.
    dialog.addEventListener("click", function (ev) {
      if (ev.target === dialog) dialog.close();
    });
    doc.addEventListener("keydown", function (ev) {
      var typing = /^(INPUT|TEXTAREA|SELECT)$/.test(ev.target.tagName) || ev.target.isContentEditable;
      if ((ev.key === "/" && !typing) || ((ev.metaKey || ev.ctrlKey) && ev.key.toLowerCase() === "k")) {
        ev.preventDefault();
        openSearch();
      }
    });
  }

  // ── Mermaid diagrams ────────────────────────────────────────────────────

  var diagrams = Array.prototype.slice.call(doc.querySelectorAll("pre.mermaid"));
  diagrams.forEach(function (pre) {
    pre.dataset.source = pre.textContent;
  });
  var mermaidLoading = null;

  function loadMermaid() {
    if (!mermaidLoading) {
      mermaidLoading = new Promise(function (resolve, reject) {
        var script = doc.createElement("script");
        script.src = ROOT + "vendor/mermaid.min.js";
        script.onload = function () {
          resolve(window.mermaid);
        };
        script.onerror = reject;
        doc.head.appendChild(script);
      });
    }
    return mermaidLoading;
  }

  function token(name) {
    return getComputedStyle(root).getPropertyValue(name).trim();
  }

  // Draws (or draws again, after a theme change) every diagram in the
  // site's colours.
  function drawDiagrams() {
    if (!diagrams.length) return;
    loadMermaid()
      .then(function (mermaid) {
        var dark = currentTheme() === "dark";
        mermaid.initialize({
          startOnLoad: false,
          securityLevel: "strict",
          theme: "base",
          fontFamily: token("--display"),
          themeVariables: {
            darkMode: dark,
            background: token("--surface"),
            primaryColor: token("--sunken"),
            primaryTextColor: token("--ink"),
            primaryBorderColor: token("--ink-soft"),
            secondaryColor: token("--surface"),
            tertiaryColor: token("--paper"),
            lineColor: token("--ink-soft"),
            textColor: token("--ink"),
            mainBkg: token("--sunken"),
            nodeBorder: token("--ink-soft"),
            clusterBkg: token("--paper"),
            clusterBorder: token("--rule"),
            edgeLabelBackground: token("--surface"),
            actorBkg: token("--accent"),
            actorBorder: token("--accent"),
            actorTextColor: token("--on-accent"),
            actorLineColor: token("--rule"),
            signalColor: token("--ink"),
            signalTextColor: token("--ink"),
            labelBoxBkgColor: token("--sunken"),
            labelBoxBorderColor: token("--ink-soft"),
            labelTextColor: token("--ink"),
            loopTextColor: token("--ink"),
            noteBkgColor: dark ? token("--code-hl") : "#fbefc8",
            noteTextColor: token("--ink"),
            noteBorderColor: token("--spark"),
            activationBkgColor: token("--spark"),
            activationBorderColor: token("--ink-soft"),
            sequenceNumberColor: token("--on-accent"),
            fontSize: "16px",
          },
          sequence: { useMaxWidth: true, mirrorActors: false, messageMargin: 30, width: 104, height: 40, actorMargin: 16, noteMargin: 8 },
          flowchart: { useMaxWidth: true, curve: "basis" },
          state: { useMaxWidth: true },
        });
        diagrams.forEach(function (pre) {
          pre.removeAttribute("data-processed");
          pre.textContent = pre.dataset.source;
        });
        return mermaid.run({ nodes: diagrams }).then(function () {
          // Mermaid lets a diagram shrink with the column. Below about
          // three quarters of the size it was drawn at, the text gets too
          // small: from there on the diagram scrolls sideways instead.
          diagrams.forEach(function (pre) {
            var svg = pre.querySelector("svg");
            var drawn = svg ? parseFloat(svg.style.maxWidth) : 0;
            if (drawn) svg.style.minWidth = Math.round(drawn * 0.72) + "px";
          });
        });
      })
      .catch(function () {
        // The source stays on the page, readable.
      });
  }
  drawDiagrams();
})();
