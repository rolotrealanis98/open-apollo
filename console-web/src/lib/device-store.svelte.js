/**
 * Svelte 5 reactive store backed by the mixer daemon WebSocket.
 * Auto-populates from device tree and syncs bidirectionally.
 *
 * Uses .svelte.js extension for $state/$derived runes at module level.
 */
import { SvelteMap } from "svelte/reactivity";
import * as ws from "./ws-client.js";

// ── Reactive state ──────────────────────────────────────────
// SvelteMap gives reliable reactivity for dynamic-key access — plain $state({})
// proxies don't notify derivations that read non-existent keys first.
const tree = new SvelteMap();
// Dev-time inspector — open DevTools and type window.__tree.get("...") / .size
if (typeof window !== "undefined") window.__tree = tree;
let connectionState = $state("disconnected");
let deviceInfo = $state({ type: "", name: "", inputs: 0, outputs: 0, preamps: 0 });

const subscribed = new Set();

// ── Public API ──────────────────────────────────────────────

export function getTree() { return tree; }
export function getConnectionState() { return connectionState; }
export function getDeviceInfo() { return deviceInfo; }

export function getValue(path) {
  return tree.get(path);
}

/** Set a value — optimistic update + send to daemon */
export function setValue(path, value) {
  tree.set(path, value);
  ws.set(path, value);
}

/** Subscribe to a path for live updates */
export function subscribePath(path) {
  if (subscribed.has(path)) return;
  subscribed.add(path);
  ws.subscribe(path);
}

export function subscribePaths(paths) {
  for (const p of paths) subscribePath(p);
}

/** Force a retry after fatal give-up */
export function retryConnection() {
  connectionState = "connecting";
  ws.retryNow();
}

/** Connect to the daemon */
export function connectToDaemon(host = "localhost", port = 4721) {
  connectionState = "connecting";

  ws.connect(`ws://${host}:${port}`, {
    onConnect: () => {
      connectionState = "connected";
      ws.getDeviceTree();
    },

    onDisconnect: () => {
      // Distinguish auto-reconnecting from idle. ws-client schedules a retry
      // unless disconnect() was called manually, so we surface "reconnecting".
      connectionState = "reconnecting";
      // Clear so re-subscription via tree:init → _subscribeDefaults re-fires.
      // Tree itself kept — avoids flicker during short reconnects; refilled by tree:init.
      subscribed.clear();
    },

    onFatal: () => {
      connectionState = "failed";
    },

    onMessage: (path, data, params) => {
      if (params.message_id === "tree:init") {
        _populateTree(data, "/devices");
        _detectDevice();
        _subscribeDefaults();
      } else if (path && data !== undefined) {
        if (path.endsWith("/DimOn/value") || path.endsWith("/Mute/value")) {
          console.log("[store] push:", path, "=", data);
        }
        tree.set(path, data);
      }
    },
  });
}

// ── Internal ────────────────────────────────────────────────

// Daemon tree shape: every node = { properties: {<name>:{type,value,...}}, children: {<key>:<node>} }.
// Canonical paths: "<prefix>/<child-key>" for children, "<prefix>/<prop-name>/value" for values.
function _populateTree(node, prefix) {
  if (!node || typeof node !== "object") return;

  const props = node.properties;
  if (props && typeof props === "object") {
    for (const [pname, pdata] of Object.entries(props)) {
      if (pdata && typeof pdata === "object" && "value" in pdata) {
        tree.set(`${prefix}/${pname}/value`, pdata.value);
      }
    }
  }

  const children = node.children;
  if (children && typeof children === "object") {
    for (const [key, child] of Object.entries(children)) {
      _populateTree(child, `${prefix}/${key}`);
    }
  }
}

function _detectDevice() {
  let inputs = 0, outputs = 0;
  const preampIdx = new Set();
  for (const path of tree.keys()) {
    const im = path.match(/\/devices\/0\/inputs\/(\d+)\//);
    if (im) inputs = Math.max(inputs, parseInt(im[1]) + 1);
    const om = path.match(/\/devices\/0\/outputs\/(\d+)\//);
    if (om) outputs = Math.max(outputs, parseInt(om[1]) + 1);
    const pm = path.match(/\/inputs\/\d+\/preamps\/(\d+)\//);
    if (pm) preampIdx.add(pm[1]);
  }
  deviceInfo = {
    type: tree.get("/devices/0/Type/value") || "Apollo",
    name: tree.get("/devices/0/DeviceName/value") || "Apollo x4",
    inputs, outputs,
    preamps: preampIdx.size,
  };
  console.log("[store] device:", $state.snapshot(deviceInfo), "paths:", tree.size);
}

function _subscribeDefaults() {
  // Preamp + input channels
  for (let i = 0; i < 4; i++) {
    const pre = `/devices/0/inputs/${i}/preamps/0`;
    subscribePaths([
      `${pre}/Gain/value`, `${pre}/48V/value`, `${pre}/Pad/value`,
      `${pre}/LowCut/value`, `${pre}/Phase/value`, `${pre}/InputSource/value`,
    ]);
    const inp = `/devices/0/inputs/${i}`;
    subscribePaths([
      `${inp}/Gain/value`, `${inp}/Pan/value`,
      `${inp}/Solo/value`, `${inp}/Mute/value`,
      `${inp}/FaderLevelTapered/value`,
      `${inp}/IOType/value`,
      `${inp}/meters/0/MeterLevel/value`, `${inp}/meters/0/MeterPeakLevel/value`,
    ]);
  }
  // S/PDIF (inputs 4-5)
  for (let i = 4; i < 6; i++) {
    const inp = `/devices/0/inputs/${i}`;
    subscribePaths([
      `${inp}/Gain/value`, `${inp}/Pan/value`,
      `${inp}/Solo/value`, `${inp}/Mute/value`,
      `${inp}/FaderLevelTapered/value`,
      `${inp}/meters/0/MeterLevel/value`, `${inp}/meters/0/MeterPeakLevel/value`,
    ]);
  }
  // Virtual (inputs 6-13)
  for (let i = 6; i < 14; i++) {
    const inp = `/devices/0/inputs/${i}`;
    subscribePaths([
      `${inp}/Gain/value`, `${inp}/Pan/value`,
      `${inp}/Solo/value`, `${inp}/Mute/value`,
      `${inp}/FaderLevelTapered/value`,
      `${inp}/meters/0/MeterLevel/value`, `${inp}/meters/0/MeterPeakLevel/value`,
    ]);
  }
  // Monitor
  const mon = `/devices/0/outputs/18`;
  subscribePaths([
    `${mon}/CRMonitorLevelTapered/value`, `${mon}/CRMonitorLevel/value`,
    `${mon}/Mute/value`, `${mon}/MixToMono/value`, `${mon}/DimOn/value`,
    `${mon}/meters/0/MeterLevel/value`, `${mon}/meters/1/MeterLevel/value`,
    `${mon}/meters/0/MeterPeakLevel/value`, `${mon}/meters/1/MeterPeakLevel/value`,
  ]);
  subscribePaths([
    "/SampleRate/value", "/ClockSource/value",
    "/TotalDSPLoad/value", "/TotalPGMLoad/value", "/TotalMEMLoad/value",
    "/devices/0/DeviceName/value",
  ]);
}
