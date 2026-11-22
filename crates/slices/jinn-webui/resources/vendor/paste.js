//! jinn webui — the composer's image attach helper.
//!
//! Datastar's expressions are synchronous, so nothing declarative can
//! run the `FileReader` a clipboard paste or a file pick needs. This is
//! the one bespoke script the page loads besides the Datastar bundle.
//!
//! It owns two jobs and no state:
//!
//!   1. Turn picked/pasted files into `[{name, contents, mime}]` — the
//!      exact shape the bundle's own file binding produces — and hand
//!      them to the page with a `jinnaddimages` document event. The
//!      markup's `data-on:jinnaddimages__document` expression merges
//!      them into the `images` signal, so the signal keeps a single
//!      writer (Datastar) and append semantics survive.
//!   2. Redraw the `image-chips` strip whenever the bundle announces a
//!      signal write (`datastar-signal-patch`). Chips are plain DOM;
//!      each ✕ carries a `data-on:click` splice expression the bundle
//!      initializes itself when the chip is added.
//!
//! The contract ids below are asserted against `contract.rs` by a Rust
//! drift test, so a renamed id fails the build rather than silently
//! leaving paste dead.
(() => {
  "use strict";

  const CHIPS_ID = "image-chips";
  const FILE_INPUT_ID = "image-file-input";
  const ADD_EVENT = "jinnaddimages";
  const SIGNAL = "images";

  const dataUrlByteLength = (url) => {
    if (typeof url !== "string") return 0;
    const comma = url.indexOf(",");
    if (comma < 0) return 0;
    const payload = url.slice(comma + 1);
    const padding = payload.endsWith("==") ? 2 : payload.endsWith("=") ? 1 : 0;
    return Math.max(0, Math.floor((payload.length * 3) / 4) - padding);
  };

  const labelFor = (entry) => {
    const name = entry && typeof entry.name === "string" ? entry.name : "";
    if (name) return name;
    const size = dataUrlByteLength(entry && entry.contents);
    return size > 0 ? `${Math.round(size / 1024)} KiB image` : "image";
  };

  /** Reads the bundle's data-url shape out of one File. */
  const readFile = (file) =>
    new Promise((resolve) => {
      const reader = new FileReader();
      reader.onload = () => {
        const result = typeof reader.result === "string" ? reader.result : "";
        const comma = result.indexOf(",");
        if (comma < 0) {
          resolve(null);
          return;
        }
        resolve({
          name: file.name || "",
          mime: file.type || "",
          contents: result.slice(comma + 1),
        });
      };
      reader.onloadend = () => resolve(null);
      reader.readAsDataURL(file);
    });

  const imageFiles = (list) =>
    [...(list || [])].filter(
      (file) => file && typeof file.type === "string" && file.type.startsWith("image/"),
    );

  /** Converts a FileList to the wire shape and asks the page to merge
   * them into the signal. */
  window.jinnAttachFiles = (list) => {
    const files = imageFiles(list);
    if (files.length === 0) return true;
    Promise.all(files.map(readFile)).then((entries) => {
      const images = entries.filter(Boolean);
      if (images.length === 0) return;
      document.dispatchEvent(
        new CustomEvent(ADD_EVENT, { detail: { images } }),
      );
    });
    return true;
  };

  const chip = (entry, index) => {
    const node = document.createElement("span");
    node.className = "chip";
    node.dataset.imageIndex = String(index);
    const thumb = document.createElement("img");
    thumb.alt = "";
    thumb.src =
      typeof entry === "string"
        ? entry
        : entry && typeof entry.contents === "string"
          ? `data:${entry.mime || "image/png"};base64,${entry.contents}`
          : "";
    node.append(thumb);
    const name = document.createElement("span");
    name.className = "chip-name";
    name.textContent = labelFor(entry);
    node.append(name);
    const remove = document.createElement("button");
    remove.type = "button";
    remove.className = "chip-x";
    remove.textContent = "\u00d7";
    remove.setAttribute("aria-label", "Remove image");
    // The splice expression runs against the live signal; the bundle's
    // MutationObserver initializes this attribute when the chip is
    // appended, so the proxy's set traps publish the new array.
    remove.setAttribute("data-on:click", `$images.splice(${index}, 1)`);
    node.append(remove);
    return node;
  };

  /** Redraws the strip from a signal value — called by the composer's
   * data-effect, so the strip is owned by one render path. */
  window.jinnRenderChips = (images) => {
    const strip = document.getElementById(CHIPS_ID);
    if (!strip) return;
    strip.replaceChildren(...images.map((entry, index) => chip(entry, index)));
  };

  // Screenshot flow: paste anywhere on the page.
  document.addEventListener("paste", (event) => {
    const files = event.clipboardData && event.clipboardData.files;
    if (imageFiles(files).length === 0) return;
    event.preventDefault();
    window.jinnAttachFiles(files);
  });

  // The visible attach label forwards clicks to the hidden file input
  // (the photo-library picker on phones).
  document.addEventListener("click", (event) => {
    const label = event.target && event.target.closest && event.target.closest(".attach");
    if (!label || event.target === document.getElementById(FILE_INPUT_ID)) return;
    const input = document.getElementById(FILE_INPUT_ID);
    if (input) input.click();
  });
})();
