  // The theme a shell document paints in. Linked from every document's
  // <head>, ahead of first paint: a page that resolved its theme after
  // the body rendered would show the dark default for a frame before
  // turning light.
  //
  // The pick lives in this origin's localStorage, next to the
  // search-engine pick — the settings page writes it, every shell
  // document reads it, and no command channel is needed because storage
  // is origin-scoped. The app keeps its own copy (theme.json) only for
  // what a document cannot do: the window icon and the colour a view
  // paints before its first frame.
  (function () {
    // The slugs tokens.css declares a block for. `theme.rs` holds this
    // list to the one in `Theme::ALL` — a slug only one side knows is a
    // page with no palette.
    var THEMES = ["glacier", "light", "lilac"];
    var KEY = "cryo.theme";
    // The one door from stored text to a theme (AGENTS §6): anything
    // unrecognized reads as the default, so a hand-edited pick cannot
    // leave a document with no palette at all.
    function resolve(pick) {
      return THEMES.indexOf(pick) === -1 ? THEMES[0] : pick;
    }
    function stored() {
      // A document whose storage area is unreachable still paints.
      try {
        return localStorage.getItem(KEY);
      } catch (e) {
        return null;
      }
    }
    function paint(slug) {
      document.documentElement.dataset.theme = slug;
      // Page art follows the palette through --mark-url; the favicon is
      // the one image CSS cannot reach, so it is swapped by hand. The
      // link sits above this script, so it is already parsed.
      var icon = document.querySelector('link[rel="icon"]');
      if (icon) icon.href = "/theme/icon-" + slug + ".svg";
    }
    // How the settings page applies its own pick, and how the app sets
    // the theme on a document it has the answer for.
    window.__cryo_theme = function (pick) {
      paint(resolve(pick));
    };
    paint(resolve(stored()));
    // Another shell document changed the pick. The storage event reaches
    // every other document on this origin, so an open new-tab page or
    // the chrome strip restyles without being reloaded.
    window.addEventListener("storage", function (e) {
      if (!e.key || e.key === KEY) paint(resolve(stored()));
    });
  })();
