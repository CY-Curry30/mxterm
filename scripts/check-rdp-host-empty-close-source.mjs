import { readFileSync } from "node:fs";
import assert from "node:assert/strict";

const host = readFileSync("src-tauri/rdp-host/HostWindow.cs", "utf8");
const chrome = readFileSync("src-tauri/rdp-host/HostChrome.cs", "utf8");
const bridge = readFileSync("src-tauri/src/rdp/winforms.rs", "utf8");
assert.match(host, /case "close":[\s\S]*bool exiting = sessions.Count == 0;[\s\S]*if \(exiting\) Close\(\);[\s\S]*exiting = exiting/);
assert.match(host, /internal void CloseTab\(Session session\)[\s\S]*CloseSession\(session.Id\);[\s\S]*if \(sessions.Count == 0\) Close\(\)/);
assert.match(chrome, /remove.Click \+= delegate \{ if \(ordered.Contains\(session\)\) close\(session\); \}/);
assert.match(chrome, /MouseButtons.Middle && ordered.Contains\(session\)\) close\(session\)/);
assert.match(bridge, /value\["exiting"\] == true[\s\S]*self.alive.store\(false/);
console.log("RDP host empty-close source check passed.");
