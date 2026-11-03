import assert from "node:assert/strict";
import { spawn, execFile } from "node:child_process";
import { promisify } from "node:util";
import { createInterface } from "node:readline";
import { once } from "node:events";

const executable = process.argv[2];
assert.ok(executable, "Pass the built mxterm-rdp.exe path");
const { stdout } = await promisify(execFile)(executable, ["--self-test"], { timeout: 20000, windowsHide: true });
assert.ok(stdout.split(/\r?\n/).filter(Boolean).map(JSON.parse).some(v => v.event === "self_test" && v.ok));

async function check(exitByEof) {
  const child = spawn(executable, ["--stdio"], { windowsHide: true, stdio: ["pipe", "pipe", "ignore"] });
  const exited = once(child, "exit");
  const timer = setTimeout(() => child.kill(), 15000);
  const lines = createInterface({ input: child.stdout })[Symbol.asyncIterator]();
  const next = async () => {
    const line = await lines.next();
    assert.ok(!line.done, "helper exited before acknowledgement");
    return JSON.parse(line.value);
  };
  try {
    const ready = await next();
    assert.equal(ready.event, "ready"); assert.equal(ready.version, 1); assert.ok(ready.hwnd > 0);
    child.stdin.write(JSON.stringify({ id: 1, command: "activate", session_id: "missing" }) + "\n");
    assert.deepEqual(await next(), { id: 1, ok: false, code: "rdp_host_command_failed", hresult: -2146233079 });
    child.stdin.write(JSON.stringify({ id: 2, command: "unknown" }) + "\n");
    const rejected = await next(); assert.equal(rejected.id, 2); assert.equal(rejected.ok, false);
    if (exitByEof) child.stdin.end();
    else {
      child.stdin.write(JSON.stringify({ id: 3, command: "shutdown" }) + "\n");
      const reply = await next(); assert.equal(reply.id, 3); assert.equal(reply.ok, true);
    }
    const [code] = await exited; assert.equal(code, 0);
  } finally { clearTimeout(timer); child.kill(); }
}
await check(false);
await check(true);
console.log("RDP AxHost control, IPC correlation, shutdown and parent EOF checks passed.");
