/*
 * canvas-telemetry.js — jinn-authored, injected into every served
 * artifact alongside the reload snippet.
 *
 * Captures interaction events and delivers them to the jinn canvas
 * server, which accumulates them until the next user chat message
 * carries them to the agent. Clicks on elements marked `data-metric`
 * post automatically; anything bespoke goes through
 * `window.canvas.track(name, props)`.
 *
 * Deliberately absent: batching, cookies, identity, dwell time, scroll
 * or visibility milestones. Every post is fire-and-forget — telemetry
 * must never break the page.
 */
(function () {
  "use strict";

  var ENDPOINT = "/metrics";

  // sendBeacon survives tab close and page unload; the fetch fallback
  // needs `keepalive` for the same reason. Wrapped in a Blob so the
  // beacon carries an explicit application/json content type.
  function post(event) {
    var payload = JSON.stringify(event);
    try {
      if (
        typeof navigator === "object" &&
        typeof navigator.sendBeacon === "function" &&
        typeof Blob === "function"
      ) {
        if (
          navigator.sendBeacon(
            ENDPOINT,
            new Blob([payload], { type: "application/json" })
          )
        ) {
          return;
        }
      }
    } catch (err) {
      /* fall through to fetch */
    }
    try {
      fetch(ENDPOINT, {
        method: "POST",
        headers: { "Content-Type": "application/json" },
        body: payload,
        keepalive: true,
      }).catch(function () {
        /* unreachable server is not the page's problem */
      });
    } catch (err) {
      /* telemetry never breaks the page */
    }
  }

  window.canvas = {
    // The manual escape hatch: name the event, attach any props the
    // agent wants echoed back verbatim.
    track: function (name, props) {
      post({ name: String(name), at: Date.now(), props: props || {} });
    },
  };

  // One delegated listener, capture phase, so page code calling
  // stopPropagation cannot suppress the report. A click on anything
  // inside a `[data-metric]` element counts; `data-metric` holds the
  // event name and any `data-metric-*` attributes become props
  // (prefix stripped).
  document.addEventListener(
    "click",
    function (ev) {
      var target = ev.target;
      if (!target || typeof target.closest !== "function") {
        return;
      }
      var el = target.closest("[data-metric]");
      if (!el) {
        return;
      }
      var props = {};
      for (var i = 0; i < el.attributes.length; i++) {
        var attr = el.attributes[i];
        if (attr.name.indexOf("data-metric-") === 0) {
          props[attr.name.slice("data-metric-".length)] = attr.value;
        }
      }
      post({
        name: el.getAttribute("data-metric") || "unnamed",
        at: Date.now(),
        props: props,
      });
    },
    true
  );
})();
