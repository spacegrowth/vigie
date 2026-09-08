import { mount } from "svelte";
import "./app.css";
import App from "./App.svelte";
import TrayPopover from "./views/TrayPopover.svelte";

// Two windows share this one frontend bundle (see src-tauri/tauri.conf.json
// and tray.rs): the main window loads plain `index.html`, the tray popover
// loads `index.html#/tray`. Route on the hash rather than shipping a second
// HTML entry point.
const isTrayPopover = location.hash === "#/tray";

if (isTrayPopover) {
  // The popover window is configured `transparent: true` so only its own
  // rounded card + shadow shows, not a solid rectangle — but app.css's
  // `body { background: var(--surface) }` is written for the main window
  // and would paint over that. Mark this document so app.css can clear
  // the background for this window only (see the `[data-route="tray"]`
  // rule there) — the main window never sets this attribute, so its own
  // opaque body is untouched.
  document.documentElement.dataset.route = "tray";
}

const app = mount(isTrayPopover ? TrayPopover : App, {
  target: document.getElementById("app")!,
});

export default app;
