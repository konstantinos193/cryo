  // Mirrors `normalize_nav_input` in shell::chrome so this box and the
  // address bar read the same text the same way. Keep the two in step.
  function destination(raw) {
    var s = raw.trim();
    if (!s) return null;
    // `javascript://x%0Aalert(1)` passes a naive scheme test, and this page
    // runs on cryo's own origin.
    if (/^javascript:/i.test(s)) return null;
    if (s.indexOf("://") !== -1 || /^(about|data|file):/i.test(s)) return s;
    // A pasted Windows path becomes a file URL, like the Rust side's
    // local_path_url, or the drive letter would read as a host.
    var asPath = s.replace(/\\/g, "/");
    if (/^[a-zA-Z]:[\\/]/.test(s)) return "file:///" + encodeURI(asPath);
    if (s.indexOf("\\\\") === 0) return "file://" + encodeURI(asPath.slice(2));
    if (s.indexOf(" ") === -1 && s.indexOf(".") !== -1) return "https://" + s;
    var e = engine();
    return e.template.replace("%s", encodeURIComponent(s));
  }

  // The search-engine pick is a slug or a custom URL template in the
  // shell origin's localStorage ("cryo.searchEngine", written by
  // /settings); the slug to engine map arrives over fetch("/engines").
  // Until it lands, or if it never does, the default engine answers.
  var engines = null;
  function engine() {
    var pick = localStorage.getItem("cryo.searchEngine") || "duckduckgo";
    var e = engines && engines[pick];
    if (e) return {name: e.name, template: e.prefix + "%s"};
    if (pick.indexOf("://") !== -1 && pick.indexOf("%s") !== -1) {
      return {name: pick.split("://")[1].split("/")[0] || "custom",
              template: pick};
    }
    return {name: "DuckDuckGo", template: "https://duckduckgo.com/?q=%s"};
  }

  var box = document.getElementById("q");
  var shell = document.getElementById("box");
  document.getElementById("go").addEventListener("submit", function (e) {
    e.preventDefault();
    var url = destination(box.value);
    if (url) location.href = url;
  });
  box.addEventListener("input", function () {
    shell.classList.toggle("filled", box.value.trim() !== "");
  });
  box.focus();

  function showEngine() {
    document.getElementById("chip-name").textContent = engine().name;
  }
  showEngine();

  fetch("/engines").then(function (r) { return r.json(); }).then(function (list) {
    engines = {};
    list.forEach(function (e) { engines[e.slug] = e; });
    showEngine();
  }).catch(function () {});

  // ---- Speed dial ----------------------------------------------------
  // The shipped list seeds the row, and every add or remove after that
  // is kept in this origin's localStorage. The chrome strip and the
  // omnibox still read the shipped list, so an edit here changes this
  // page and nothing else.
  var STORE = "cryo.dials";
  var MAX = 12;
  var dialWrap = document.getElementById("dial");
  var scrim = document.getElementById("scrim");
  var editor = document.getElementById("editor");
  var nameField = document.getElementById("e-name");
  var urlField = document.getElementById("e-url");
  var errorLine = document.getElementById("e-error");
  var resetButton = document.getElementById("reset");
  var shipped = [];
  var dials = [];
  var painted = false;

  // An entry is only ever stored once it survives this: a name, and an
  // http or https address the URL parser agrees with.
  function entry(name, url) {
    var text = String(name == null ? "" : name).trim().slice(0, 24);
    var raw = String(url == null ? "" : url).trim();
    if (!text || !raw) return null;
    if (raw.indexOf("://") === -1) raw = "https://" + raw;
    try {
      var parsed = new URL(raw);
      if (parsed.protocol !== "https:" && parsed.protocol !== "http:") return null;
      return {name: text, url: parsed.href};
    } catch (err) {
      return null;
    }
  }

  function stored() {
    try {
      var raw = localStorage.getItem(STORE);
      if (!raw) return null;
      var list = JSON.parse(raw);
      if (!Array.isArray(list)) return null;
      return list.map(function (d) { return d && entry(d.name, d.url); })
                 .filter(Boolean).slice(0, MAX);
    } catch (err) {
      return null;
    }
  }

  function keep() {
    try {
      localStorage.setItem(STORE, JSON.stringify(dials));
    } catch (err) {}
    resetButton.hidden = false;
  }

  // A stable hue per site, kept inside the cold half of the wheel so the
  // tiles read as one set rather than a paint chart. How light that hue
  // is worn belongs to the theme, not here — a chip mixed dark in this
  // script would stay dark on a light page.
  function accent(name) {
    var h = 0;
    for (var i = 0; i < name.length; i++) {
      h = (h * 31 + name.charCodeAt(i)) % 1021;
    }
    var hue = 168 + (h % 92);
    return {chip: "hsl(" + hue + " var(--tint-bg))", glyph: "hsl(" + hue + " var(--tint-fg))"};
  }

  // The site's own icon, straight from its origin: the app keeps no icon
  // store a content view could read. Sent without a referrer so the
  // request says nothing about where it came from, and the engine's HTTP
  // cache answers every new tab after the first. A site with no
  // /favicon.ico simply keeps its initial.
  function favicon(url, dot) {
    var img = document.createElement("img");
    img.alt = "";
    img.referrerPolicy = "no-referrer";
    img.addEventListener("load", function () {
      if (img.naturalWidth > 0) dot.classList.add("iconed");
    });
    img.src = new URL("/favicon.ico", url).href;
    return img;
  }

  function slotFor(d, i) {
    var slot = document.createElement("div");
    slot.className = "slot";
    slot.style.animationDelay = (0.24 + i * 0.045).toFixed(2) + "s";

    var a = document.createElement("a");
    a.className = "tile";
    a.href = d.url;
    var tone = accent(d.name);
    a.style.setProperty("--chip", tone.chip);
    a.style.setProperty("--glyph", tone.glyph);

    var dot = document.createElement("span");
    dot.className = "dot";
    dot.textContent = d.name.charAt(0);
    dot.appendChild(favicon(d.url, dot));
    var label = document.createElement("span");
    label.className = "name";
    label.textContent = d.name;
    a.append(dot, label);
    if (i < 9) {
      var idx = document.createElement("span");
      idx.className = "idx";
      idx.textContent = i + 1;
      a.appendChild(idx);
    }

    var kill = document.createElement("button");
    kill.type = "button";
    kill.className = "kill";
    kill.textContent = "×";
    kill.title = "Remove";
    kill.setAttribute("aria-label", "Remove " + d.name);
    kill.addEventListener("click", function () {
      dials.splice(i, 1);
      keep();
      paint();
    });

    slot.append(a, kill);
    return slot;
  }

  function adder() {
    var b = document.createElement("button");
    b.type = "button";
    b.className = "tile add";
    b.style.animationDelay = (0.24 + dials.length * 0.045).toFixed(2) + "s";
    b.setAttribute("aria-label", "Add a tile");
    var dot = document.createElement("span");
    dot.className = "dot";
    dot.textContent = "+";
    var label = document.createElement("span");
    label.className = "name";
    label.textContent = "Add";
    b.append(dot, label);
    b.addEventListener("click", openEditor);
    return b;
  }

  function paint() {
    dialWrap.textContent = "";
    dials.forEach(function (d, i) { dialWrap.appendChild(slotFor(d, i)); });
    if (dials.length < MAX) dialWrap.appendChild(adder());
    var reachable = Math.min(dials.length, 9);
    document.getElementById("dial-max").textContent = reachable;
    document.getElementById("dial-hint").hidden = reachable < 2;
    // Only the arrival is worth staging; later repaints are edits.
    if (painted) dialWrap.classList.add("settled");
    painted = true;
  }

  // The editor is a dialog over the page. It opens empty, closes on the
  // backdrop, on Cancel and on Escape, and hands focus back to the
  // search box so the keyboard never lands nowhere.
  function openEditor() {
    scrim.hidden = false;
    nameField.value = "";
    urlField.value = "";
    urlField.classList.remove("bad");
    errorLine.hidden = true;
    nameField.focus();
  }

  function closeEditor() {
    scrim.hidden = true;
    box.focus();
  }

  editor.addEventListener("submit", function (e) {
    e.preventDefault();
    var added = entry(nameField.value, urlField.value);
    if (!added) {
      urlField.classList.add("bad");
      errorLine.hidden = false;
      (nameField.value.trim() ? urlField : nameField).focus();
      return;
    }
    dials.push(added);
    keep();
    paint();
    closeEditor();
  });
  document.getElementById("e-cancel").addEventListener("click", closeEditor);
  scrim.addEventListener("mousedown", function (e) {
    if (e.target === scrim) closeEditor();
  });
  urlField.addEventListener("input", function () {
    urlField.classList.remove("bad");
    errorLine.hidden = true;
  });

  resetButton.addEventListener("click", function () {
    try {
      localStorage.removeItem(STORE);
    } catch (err) {}
    dials = shipped.slice();
    resetButton.hidden = true;
    paint();
  });

  // A stored list paints at once. The shipped one is still fetched,
  // because it is what "restore" restores to.
  var own = stored();
  if (own) {
    dials = own;
    resetButton.hidden = false;
    paint();
  }
  fetch("/dials").then(function (r) { return r.json(); }).then(function (list) {
    shipped = list.map(function (d) { return entry(d.name, d.url); }).filter(Boolean);
    if (!own) {
      dials = shipped.slice();
      paint();
    }
  }).catch(function () { if (!own) paint(); });

  // ---- Clock, keys, frost ---------------------------------------------
  // The clock shows seconds, so it re-reads the time on the second
  // rather than every 1000ms from whenever the page happened to load,
  // which drifts and skips a digit now and then. A hidden tab pauses
  // its animations, and this is the one timer left running there.
  var clockTime = document.getElementById("clock-time");
  var clockDate = document.getElementById("clock-date");
  function tick() {
    var now = new Date();
    clockTime.textContent = now.toLocaleTimeString(
      [], {hour: "2-digit", minute: "2-digit", second: "2-digit"});
    clockDate.textContent =
      now.toLocaleDateString([], {weekday: "short", day: "numeric", month: "short"});
    setTimeout(tick, 1000 - now.getMilliseconds() + 20);
  }
  tick();

  // Nothing on this page needs to move while it sits in a background tab.
  document.addEventListener("visibilitychange", function () {
    document.body.classList.toggle("still", document.hidden);
  });

  // Keyboard: "/" reaches the box from anywhere, Escape backs out of
  // whatever is open, and a digit opens that tile. Modified keys belong
  // to the chrome's accelerators, so they pass straight through.
  document.addEventListener("keydown", function (e) {
    if (e.ctrlKey || e.altKey || e.metaKey) return;
    var typing = e.target.tagName === "INPUT";
    if (e.key === "Escape") {
      if (e.target === box) {
        if (box.value) {
          box.value = "";
          shell.classList.remove("filled");
        } else {
          box.blur();
        }
      } else if (!scrim.hidden) {
        closeEditor();
      }
      return;
    }
    if (typing) return;
    if (e.key === "/") {
      e.preventDefault();
      box.focus();
      return;
    }
    if (e.key >= "1" && e.key <= "9") {
      var pick = dials[Number(e.key) - 1];
      if (pick) location.href = pick.url;
    }
  });

  // The pointer drags the ice field along and blooms frost under itself.
  // One rAF per frame of movement, and nothing at all when the pointer is
  // still or motion is turned down.
  if (!matchMedia("(prefers-reduced-motion: reduce)").matches) {
    var field = document.getElementById("field");
    var lattice = document.getElementById("lattice");
    var queued = false, at = null;
    window.addEventListener("pointermove", function (e) {
      at = e;
      if (queued) return;
      queued = true;
      requestAnimationFrame(function () {
        queued = false;
        var dx = (at.clientX / window.innerWidth - 0.5) * 26;
        var dy = (at.clientY / window.innerHeight - 0.5) * 18;
        field.style.setProperty("--px", dx.toFixed(1) + "px");
        field.style.setProperty("--py", dy.toFixed(1) + "px");
        lattice.style.setProperty("--mx", at.clientX + "px");
        lattice.style.setProperty("--my", at.clientY + "px");
        lattice.classList.add("lit");
      });
    });
    // Leaving the window takes the frost with it, rather than freezing a
    // bright patch wherever the pointer happened to exit.
    document.addEventListener("pointerleave", function () {
      lattice.classList.remove("lit");
    });
  }
