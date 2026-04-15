// sloos.js - zero-dependency vanilla JS client for sloos.
//
// Usage:
//   sloos("/sloos", "form.sloos-form");
//
// For each matching <form> element, this:
//   1. Disables submission until the PoW is computed.
//   2. Fetches a nonce + difficulty from the sloos GET endpoint.
//   3. Solves the hashcash-style PoW in the background.
//   4. Populates hidden `_sloos_nonce` and `_sloos_pow` fields in the form
//      (creating them if needed).
//   5. Re-enables the form's submit control.
//
// If the form's action is not set, it's set to the sloos endpoint.

(function (global) {
  "use strict";

  function hex(bytes) {
    let s = "";
    for (let i = 0; i < bytes.length; i++) {
      s += bytes[i].toString(16).padStart(2, "0");
    }
    return s;
  }

  function hexToBytes(h) {
    const out = new Uint8Array(h.length / 2);
    for (let i = 0; i < out.length; i++) {
      out[i] = parseInt(h.substr(i * 2, 2), 16);
    }
    return out;
  }

  function leadingZeroBits(bytes) {
    let count = 0;
    for (let i = 0; i < bytes.length; i++) {
      const b = bytes[i];
      if (b === 0) {
        count += 8;
      } else {
        // Math.clz32 returns leading zeros in a 32-bit integer; we want 8-bit.
        count += Math.clz32(b) - 24;
        break;
      }
    }
    return count;
  }

  async function sha256(bytes) {
    const buf = await crypto.subtle.digest("SHA-256", bytes);
    return new Uint8Array(buf);
  }

  async function solve(nonceBytes, difficulty) {
    const pow = new Uint8Array(8); // 64-bit counter, big-endian
    // We track a 64-bit counter as two 32-bit halves so we don't hit JS
    // integer precision issues for long searches.
    let lo = 0;
    let hi = 0;
    const concat = new Uint8Array(nonceBytes.length + pow.length);
    concat.set(nonceBytes, 0);
    while (true) {
      // write counter into `pow` big-endian
      pow[0] = (hi >>> 24) & 0xff;
      pow[1] = (hi >>> 16) & 0xff;
      pow[2] = (hi >>> 8) & 0xff;
      pow[3] = hi & 0xff;
      pow[4] = (lo >>> 24) & 0xff;
      pow[5] = (lo >>> 16) & 0xff;
      pow[6] = (lo >>> 8) & 0xff;
      pow[7] = lo & 0xff;
      concat.set(pow, nonceBytes.length);
      const digest = await sha256(concat);
      if (leadingZeroBits(digest) >= difficulty) {
        return hex(pow);
      }
      lo = (lo + 1) >>> 0;
      if (lo === 0) {
        hi = (hi + 1) >>> 0;
      }
    }
  }

  function ensureHiddenField(form, name) {
    let el = form.querySelector('input[name="' + name + '"]');
    if (!el) {
      el = document.createElement("input");
      el.type = "hidden";
      el.name = name;
      form.appendChild(el);
    }
    return el;
  }

  function setFormEnabled(form, enabled) {
    const controls = form.querySelectorAll(
      'button[type="submit"], input[type="submit"]',
    );
    for (const c of controls) {
      c.disabled = !enabled;
    }
    form.dataset.sloosReady = enabled ? "1" : "0";
  }

  async function prepareForm(endpoint, form) {
    setFormEnabled(form, false);
    if (!form.getAttribute("action")) {
      form.setAttribute("action", endpoint);
    }
    const resp = await fetch(endpoint, { method: "GET" });
    if (!resp.ok) {
      throw new Error("sloos GET failed: " + resp.status);
    }
    const data = await resp.json();
    const nonceBytes = hexToBytes(data.nonce);
    const powHex = await solve(nonceBytes, data.difficulty);
    ensureHiddenField(form, "_sloos_nonce").value = data.nonce;
    ensureHiddenField(form, "_sloos_pow").value = powHex;
    setFormEnabled(form, true);
  }

  function sloos(endpoint, selector) {
    const forms = document.querySelectorAll(selector);
    const promises = [];
    for (const form of forms) {
      promises.push(
        prepareForm(endpoint, form).catch((err) => {
          form.dataset.sloosError = String(err);
        }),
      );
    }
    return Promise.all(promises);
  }

  // Export. When loaded as a plain <script>, attaches to window.
  if (typeof module !== "undefined" && module.exports) {
    module.exports = sloos;
  } else {
    global.sloos = sloos;
  }
})(typeof window !== "undefined" ? window : globalThis);
