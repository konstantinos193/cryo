  var KINDS = {
    "offline": ["You are offline",
      "cryo cannot reach the network at all. Check your Wi-Fi or cable, then try again."],
    "host-not-found": ["This site cannot be found",
      "Nothing answers to that address. Check the domain for a typo."],
    "timed-out": ["The site took too long",
      "The server accepted the connection but never finished answering."],
    "connection-failed": ["The connection failed",
      "The server refused the connection, or dropped it before the page arrived."],
    "certificate-rejected": ["This connection is not private",
      "The site security certificate does not check out, so cryo stopped before sending anything."],
    "redirect-loop": ["Too many redirects",
      "The site kept forwarding cryo somewhere else and never settled on a page."],
    "malformed-response": ["cryo could not read the answer",
      "The server replied with something that is not a web page."],
    "render-process-gone": ["This tab crashed",
      "The page renderer stopped and cryo shut it down. Reloading usually brings it back."],
    "unknown": ["Something went wrong",
      "The page did not load, and the engine did not say why."],
    "web-store": ["This window cannot install extensions",
      "\"Add to Chrome\" asked the engine for the package, and the address it was sent to is a placeholder that can never resolve, so retrying will not help. cryo installs from the store in an ordinary window on Windows — a private window loads no extensions, and other platforms cannot load them at all."]
  };
  var HTTP = {
    "400": "The server rejected the request as malformed.",
    "401": "This page needs credentials cryo does not have.",
    "403": "The server understood the request and refused it.",
    "404": "The server has no page at this address.",
    "405": "The server does not allow that method on this address.",
    "408": "The server gave up waiting for the request.",
    "410": "This page was removed on purpose and is not coming back.",
    "418": "The server is a teapot and declines to brew coffee.",
    "429": "Too many requests, too fast. Wait a moment, then retry.",
    "500": "The server hit an error of its own while building the page.",
    "501": "The server does not support what was asked of it.",
    "502": "A server upstream returned something the gateway could not use.",
    "503": "The server is overloaded or down for maintenance.",
    "504": "A server upstream did not answer in time."
  };

  var q = new URLSearchParams(location.search);
  var kind = q.get("kind") || "unknown";
  var code = q.get("code") || "";

  // Only ever hand the retry button a web URL. This page sits on cryo's own
  // origin, so a page that navigated here with `url=javascript:...` would
  // otherwise get script execution on it.
  var target = "";
  try {
    var parsed = new URL(q.get("url") || "");
    if (parsed.protocol === "http:" || parsed.protocol === "https:") target = parsed.href;
  } catch (e) { /* unparseable: show no address and offer no retry */ }

  var copy = KINDS[kind] || KINDS["unknown"];
  var title = copy[0];
  var message = copy[1];
  if (kind === "http") {
    title = "HTTP " + code;
    message = HTTP[code] || "The server answered with an error and no page.";
  }

  document.title = title;
  document.getElementById("title").textContent = title;
  document.getElementById("message").textContent = message;
  if (target) {
    var targetEl = document.getElementById("target");
    targetEl.textContent = target;
    targetEl.hidden = false;
    // No retry for a Web Store install: the address is a placeholder, so the
    // button could only ever fail again. Offering it would be a lie with a
    // pointer on it.
    if (kind !== "web-store") {
      var retry = document.getElementById("retry");
      retry.hidden = false;
      retry.addEventListener("click", function () { location.replace(target); });
    }
  }
