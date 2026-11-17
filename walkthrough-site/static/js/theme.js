// Runs in <head>, after the stylesheet links and before the first paint: applies the theme the reader
// chose earlier (or `?theme=dark` / `?theme=light` in the address), so the
// page does not flash in the other one. Without a choice the system's
// setting decides, in CSS alone.
(function () {
  var root = document.documentElement;
  var chosen = null;
  try {
    var asked = new URLSearchParams(location.search).get("theme");
    if (asked === "light" || asked === "dark") {
      chosen = asked;
    } else {
      chosen = localStorage.getItem("theme");
    }
  } catch (e) {
    // No storage (private mode): the system's setting.
  }
  if (chosen === "light" || chosen === "dark") {
    root.setAttribute("data-theme", chosen);
  }

  // The code colours are two stylesheets; the one of the theme in use is on.
  window.applyCodeTheme = function () {
    var theme = root.getAttribute("data-theme");
    document.querySelectorAll("link[data-code-theme]").forEach(function (link) {
      var mine = link.getAttribute("data-code-theme");
      link.media = theme ? (theme === mine ? "all" : "not all") : "(prefers-color-scheme: " + mine + ")";
    });
  };
  window.applyCodeTheme();
})();
