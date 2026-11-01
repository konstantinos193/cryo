  // Both pickers on this page are the same control: a button that opens
  // a listbox, with one row marked as the current answer. Only what
  // happens after a pick differs, so that is the one thing a caller
  // passes in.
  // Shut every list. A click anywhere outside one does this, and so does
  // opening the other: two listboxes open at once is one of them lying
  // about where the next click goes.
  function shutAll() {
    var open = document.querySelectorAll(".xdrop.open");
    for (var i = 0; i < open.length; i++) open[i].classList.remove("open");
  }
  function picker(id, picked) {
    var drop = document.getElementById(id + "-drop");
    var lbl = document.getElementById(id + "-label");
    var pop = document.getElementById(id + "-pop");
    var current = null;
    document.getElementById(id).onclick = function (e) {
      e.stopPropagation();
      var opening = !drop.classList.contains("open");
      shutAll();
      if (opening) drop.classList.add("open");
    };
    drop.addEventListener("keydown", function (e) {
      if (e.key === "Escape") drop.classList.remove("open");
    });
    var api = {
      // Paint a value as the current answer without acting on it —
      // what the stored pick does at load.
      mark: function (value, label) {
        current = value;
        lbl.textContent = label;
        for (var i = 0; i < pop.children.length; i++) {
          var el = pop.children[i];
          el.classList.toggle("on", el.dataset.v === value);
        }
        return api;
      },
      value: function () {
        return current;
      },
      add: function (value, label) {
        var item = document.createElement("button");
        item.type = "button";
        item.className = "xdrop-item";
        item.dataset.v = value;
        item.setAttribute("role", "option");
        item.textContent = label;
        item.onclick = function (e) {
          e.stopPropagation();
          drop.classList.remove("open");
          api.mark(value, label);
          picked(value);
        };
        pop.appendChild(item);
        return api;
      },
    };
    return api;
  }
  document.addEventListener("click", shutAll);

  // ---- Theme -------------------------------------------------------
  // The pick lives in this origin's localStorage, where theme.js (in
  // every shell document's head) reads it. Applying it is three things:
  // this document now, every other open shell document through the
  // storage event, and the app — which owns the window icon and the
  // colour a fresh view paints, neither of which a page can set.
  var THEME_KEY = "cryo.theme";
  var theme = picker("theme", function (slug) {
    localStorage.setItem(THEME_KEY, slug);
    window.__cryo_theme(slug);
    location.href = "/settings/theme/" + slug;
  });
  fetch("/themes")
    .then(function (r) {
      return r.json();
    })
    .then(function (list) {
      list.forEach(function (t) {
        theme.add(t.slug, t.name);
      });
      var stored = localStorage.getItem(THEME_KEY);
      var hit =
        list.filter(function (t) {
          return t.slug === stored;
        })[0] || list[0];
      theme.mark(hit.slug, hit.name);
    })
    .catch(function () {});

  // ---- Downloads ---------------------------------------------------
  var downloadChange = document.getElementById("download-change");
  window.__cryo_download_folder = function (path, error) {
    if (path) document.getElementById("download-path").textContent = path;
    document.getElementById("download-status").textContent = error || "";
    downloadChange.disabled = !path;
    if (!path) document.getElementById("download-path").textContent = "Folder unavailable";
  };
  downloadChange.onclick = function () {
    downloadChange.disabled = true;
    location.href = "/settings/download-folder/choose";
  };
  location.href = "/settings/download-folder";

  // ---- Search ------------------------------------------------------
  // The pick is stored in this origin's localStorage — the same storage
  // chrome.html and the new-tab page read. It is either a preset slug
  // or a custom template: an http(s) URL holding %s where the encoded
  // query goes. `cryo.searchEngine.custom` remembers the last template
  // so toggling away and back doesn't lose it.
  var KEY = "cryo.searchEngine";
  var CUSTOM_KEY = "cryo.searchEngine.custom";
  var tpl = document.getElementById("engine-tpl");
  function isTemplate(v) {
    v = v.trim();
    var m = /^https?:\/\/[^\s/]+/.exec(v);
    return !!m && v.indexOf("%s") !== -1 && !/\s/.test(v);
  }
  // A preset stores its slug. "Custom…" shows the template box and
  // stores nothing until that template is valid — the previous pick
  // keeps working meanwhile.
  function engineChosen(slug) {
    tpl.hidden = slug !== "custom";
    if (slug !== "custom") {
      localStorage.setItem(KEY, slug);
      return;
    }
    if (!tpl.value) tpl.value = localStorage.getItem(CUSTOM_KEY) || "";
    if (isTemplate(tpl.value)) localStorage.setItem(KEY, tpl.value.trim());
    tpl.focus();
  }
  var engine = picker("engine", engineChosen);
  fetch("/engines")
    .then(function (r) {
      return r.json();
    })
    .then(function (list) {
      list.forEach(function (e) {
        engine.add(e.slug, e.name);
      });
      engine.add("custom", "Custom…");
      var stored = localStorage.getItem(KEY) || "duckduckgo";
      var hit = list.filter(function (e) {
        return e.slug === stored;
      })[0];
      if (hit) {
        engine.mark(hit.slug, hit.name);
        engineChosen(hit.slug);
        return;
      }
      var t = isTemplate(stored) ? stored : localStorage.getItem(CUSTOM_KEY) || "";
      if (t) tpl.value = t;
      engine.mark("custom", "Custom…");
      engineChosen("custom");
    })
    .catch(function () {});
  tpl.addEventListener("input", function () {
    if (isTemplate(tpl.value)) {
      localStorage.setItem(KEY, tpl.value.trim());
      localStorage.setItem(CUSTOM_KEY, tpl.value.trim());
    }
  });
