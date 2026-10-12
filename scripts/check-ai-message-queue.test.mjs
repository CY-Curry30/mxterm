import assert from "node:assert/strict";
import { randomUUID } from "node:crypto";
import { readFileSync } from "node:fs";
import test from "node:test";
import ts from "typescript";

function moduleUrl(source) {
  const { outputText } = ts.transpileModule(source, {
    compilerOptions: { module: ts.ModuleKind.ESNext, target: ts.ScriptTarget.ES2022 },
  });
  return `data:text/javascript;base64,${Buffer.from(outputText).toString("base64")}`;
}

const readSource = (name) => readFileSync(new URL(`../src/features/ai/${name}`, import.meta.url), "utf8");
const storeUrl = moduleUrl(readSource("aiAssistantScopeStore.ts").replace('from "react"', `from ${JSON.stringify(import.meta.resolve("react"))}`));
const store = await import(storeUrl);
const flowUrl = moduleUrl(readSource("aiMessageFlow.ts"));
const filesUrl = moduleUrl(readSource("aiFileChanges.ts"));

// Exercise the real event reducer without mounting the desktop panel. Follow
// its local function/constant dependencies through the TypeScript syntax tree.
const panel = ts.createSourceFile("AiAssistantPanel.tsx", readSource("AiAssistantPanel.tsx"), ts.ScriptTarget.Latest, true, ts.ScriptKind.TSX);
const declarations = new Map();
for (const statement of panel.statements) {
  if (ts.isFunctionDeclaration(statement) && statement.name) declarations.set(statement.name.text, statement);
  if (ts.isVariableStatement(statement)) {
    for (const declaration of statement.declarationList.declarations) {
      if (ts.isIdentifier(declaration.name)) declarations.set(declaration.name.text, statement);
    }
  }
}
const required = new Set();
function include(name) {
  const declaration = declarations.get(name);
  if (!declaration || required.has(declaration)) return;
  required.add(declaration);
  function visit(node) {
    if (ts.isIdentifier(node)) include(node.text);
    ts.forEachChild(node, visit);
  }
  visit(declaration);
}
include("applyAiStreamEventToScope");
const { applyAiStreamEventToScope } = await import(moduleUrl(`
  import { appendThinkingDelta, finishThinkingBlock } from ${JSON.stringify(flowUrl)};
  import { applyFileChangeSummaries } from ${JSON.stringify(filesUrl)};
  ${[...required].map((node) => node.getText(panel)).join("\n")}
  export { applyAiStreamEventToScope };
`));

function deferred() {
  let resolve, reject;
  const promise = new Promise((yes, no) => { resolve = yes; reject = no; });
  return { promise, resolve, reject };
}

async function flush() {
  await new Promise((resolve) => setImmediate(resolve));
}

async function harness() {
  const commandsUrl = moduleUrl(`
    // ${randomUUID()}
    let handlers;
    export function configure(next) { handlers = next; }
    export const aiChatStreamStart = (request) => handlers.start(request);
    export const aiChatStreamEnqueue = (request) => handlers.enqueue(request);
    export const aiChatStreamStop = (id) => handlers.stop(id);
    export const aiChatSessionGet = (id) => handlers.get(id);
  `);
  const commands = await import(commandsUrl);
  const queue = await import(moduleUrl(readSource("aiMessageQueue.ts")
    .replace('from "../../shared/tauri/commands"', `from ${JSON.stringify(commandsUrl)}`)
    .replace('from "./aiAssistantScopeStore"', `from ${JSON.stringify(storeUrl)}`)));
  const scope = randomUUID();
  const calls = { start: [], enqueue: [], stop: [], get: [] };
  const handlers = {
    start: async (request) => {
      const ids = request.client_ids;
      return {
        ...ids,
        user_message: {
          ...store.readAiScopeState(scope).messages.find((message) => message.id === ids.user_message_id),
          contexts: request.contexts.map((context) => ({ ...context, artifact_id: "artifact", data_url: undefined })),
        },
      };
    },
    enqueue: async () => {},
    stop: async () => {},
    get: async (id) => ({ id, messages: store.readAiScopeState(scope).messages }),
  };
  commands.configure(Object.fromEntries(Object.keys(handlers).map((kind) => [kind, (value) => {
    calls[kind].push(value);
    return handlers[kind](value);
  }])));
  const request = {
    provider_config_id: "provider", session_id: null, content: "first", contexts: [],
    host_scope: "demo-host", connection_id: "connection", model: "model",
    agent: { workspace_type: "remote", connection_id: "connection", workspace_path: "/tmp/demo", mode: "execute" },
  };
  const ensureListener = async () => {};
  const state = () => store.readAiScopeState(scope);
  function event(kind, extra = {}) {
    const stream = state().streamState;
    const payload = { kind, stream_id: stream.streamId, session_id: stream.sessionId, message_id: stream.assistantMessageId, ...extra };
    let effect;
    store.updateAiScopeState(scope, (current) => {
      const result = applyAiStreamEventToScope(current, payload);
      effect = result.effect;
      return result.state;
    });
    if (effect?.kind === "stream_ended") queue.finishAiMessageQueue(scope, effect.status, ensureListener);
  }
  const start = () => queue.startAiMessage(scope, request, ensureListener);
  const add = (content, contexts = []) => queue.queueAiMessage(scope, { ...request, content, contexts });
  const sendNow = (id = state().queuedMessages[0].id) => queue.sendQueuedAiMessageNow(scope, id, ensureListener);
  return { scope, calls, handlers, queue, request, state, event, start, add, sendNow, ensureListener };
}

test("send displays messages before listener and IPC preparation finish; early events survive acknowledgement", async () => {
  const h = await harness();
  const listener = deferred();
  const pending = h.queue.startAiMessage(h.scope, h.request, () => listener.promise);
  assert.equal(h.state().messages.length, 2);
  assert.equal(h.state().streamState.ready, false);
  assert.equal(h.calls.start.length, 0);
  h.event("chunk", { delta: "early output" });
  listener.resolve();
  await pending;
  assert.equal(h.state().messages[1].content, "early output");
  assert.equal(h.state().streamState.ready, true);
});

test("multiple queued messages automatically run in order with the original host and model", async () => {
  const h = await harness();
  await h.start();
  h.add("second");
  h.add("third");
  assert.equal(h.calls.start.length, 1);
  h.event("finished", { content: "" });
  await flush();
  h.event("finished", { content: "" });
  await flush();
  assert.deepEqual(h.calls.start.map((request) => request.content), ["first", "second", "third"]);
  assert.ok(h.calls.start.every((request) => request.connection_id === "connection" && request.model === "model"));
  assert.equal(h.state().queuedMessages.length, 0);
});

test("a follow-up sent after an early finish but before the start response is queued and delivered", async () => {
  const h = await harness();
  const acknowledgement = deferred();
  const normalStart = h.handlers.start;
  h.handlers.start = async (request) => {
    const response = await normalStart(request);
    await acknowledgement.promise;
    return response;
  };
  const pending = h.start();
  await flush();
  h.event("finished", { content: "" });
  await h.queue.startAiMessage(h.scope, { ...h.request, session_id: h.state().activeSessionId, content: "follow-up" }, h.ensureListener);
  assert.equal(h.state().queuedMessages[0].content, "follow-up");
  h.handlers.start = normalStart;
  acknowledgement.resolve();
  await pending;
  await flush();
  assert.deepEqual(h.calls.start.map((request) => request.content), ["first", "follow-up"]);
});

test("injection event before IPC acknowledgement removes the queued message exactly once and routes new output", async () => {
  const h = await harness();
  await h.start();
  h.add("inserted");
  const item = h.state().queuedMessages[0];
  const acknowledgement = deferred();
  h.handlers.enqueue = () => acknowledgement.promise;
  h.sendNow();
  await flush();
  const previous = h.state().messages[1];
  const assistant = { ...previous, id: randomUUID(), content: "", status: "streaming" };
  h.event("message_injected", { injection: {
    queue_ids: [item.id], previous_message: { ...previous, status: "complete" },
    user_messages: [{ ...h.state().messages[0], id: item.id, content: item.content }], assistant_message: assistant,
  } });
  h.event("chunk", { delta: "reply to insertion" });
  acknowledgement.resolve();
  await flush();
  assert.equal(h.state().queuedMessages.length, 0);
  assert.equal(h.state().messages.length, 4);
  assert.equal(h.state().messages[1].status, "complete");
  assert.equal(h.state().messages[3].content, "reply to insertion");
  assert.equal(h.state().streamState.assistantMessageId, assistant.id);
  assert.equal(h.calls.start.length, 1);
  assert.equal(h.calls.enqueue.length, 1);
});

test("a closed insertion racing with finish is sent as the next turn without duplication", async () => {
  const h = await harness();
  await h.start();
  h.add("late insertion");
  const acknowledgement = deferred();
  h.handlers.enqueue = () => acknowledgement.promise;
  h.sendNow();
  h.event("finished", { content: "" });
  acknowledgement.reject({ code: "ai_injection_closed", message: "closed" });
  await flush();
  assert.deepEqual(h.calls.start.map((request) => request.content), ["first", "late insertion"]);
  assert.equal(h.state().queuedMessages.length, 0);
});

test("stop retains accepted and ordinary messages and does not start another turn", async () => {
  const h = await harness();
  await h.start();
  h.add("immediate");
  h.add("ordinary");
  h.sendNow();
  await flush();
  assert.equal(h.state().queuedMessages[0].status, "accepted");
  await h.queue.stopAiMessage(h.scope);
  h.event("stopped");
  await flush();
  assert.equal(h.calls.stop.length, 1);
  assert.equal(h.calls.start.length, 1);
  assert.deepEqual(h.state().queuedMessages.map((item) => [item.content, item.status, item.immediate]), [
    ["immediate", "queued", false], ["ordinary", "queued", false],
  ]);
});

test("stop while startup is pending waits for acknowledgement and suppresses automatic continuation", async () => {
  const h = await harness();
  const acknowledgement = deferred();
  const normalStart = h.handlers.start;
  h.handlers.start = async (request) => { await acknowledgement.promise; return normalStart(request); };
  const pending = h.start();
  h.add("retained");
  await h.queue.stopAiMessage(h.scope);
  assert.equal(h.calls.stop.length, 0);
  acknowledgement.resolve();
  await pending;
  assert.equal(h.calls.stop.length, 1);
  h.event("finished", { content: "" });
  await flush();
  assert.equal(h.state().queueAutoRun, false);
  assert.equal(h.state().queuedMessages[0].content, "retained");
});

test("failed queued startup preserves the message, pauses later sends, and allows an explicit retry", async () => {
  const h = await harness();
  await h.start();
  h.add("retry me");
  h.add("later");
  const normalStart = h.handlers.start;
  h.handlers.start = async () => { throw { message: "provider unavailable" }; };
  h.event("finished", { content: "" });
  await flush();
  assert.equal(h.state().queuedMessages[0].status, "failed");
  assert.equal(h.state().queuedMessages[0].error, "provider unavailable");
  assert.equal(h.state().messages.length, 2);
  assert.equal(h.calls.start.length, 2);
  h.handlers.start = normalStart;
  h.sendNow();
  await flush();
  assert.equal(h.calls.start.length, 3);
  assert.equal(h.state().queuedMessages[0].content, "later");
});

test("stream failure pauses the queue without discarding attachments", async () => {
  const h = await harness();
  await h.start();
  const contexts = [{ id: "file", kind: "file", title: "demo.txt", content: "full text" }];
  h.add("retained", contexts);
  h.event("error", { error: "network error" });
  await flush();
  assert.equal(h.calls.start.length, 1);
  assert.deepEqual(h.state().queuedMessages[0].contexts, contexts);
});

test("queued image and text attachments reach IPC and optimistic metadata is replaced with persisted artifacts", async () => {
  const h = await harness();
  await h.start();
  const contexts = [
    { id: "image", kind: "image", title: "demo.png", content: "image", data_url: "data:image/png;base64,demo" },
    { id: "file", kind: "file", title: "demo.txt", content: "full text" },
  ];
  h.add("", contexts);
  h.event("finished", { content: "" });
  await flush();
  assert.deepEqual(h.calls.start[1].contexts, contexts);
  assert.ok(h.state().messages[2].contexts.every((context) => context.artifact_id === "artifact" && !context.data_url));
});

test("tab scopes and historical sessions retain independent queues", async () => {
  const first = await harness();
  const second = await harness();
  await first.start();
  await second.start();
  first.add("first tab queue");
  second.add("second tab queue");
  first.event("finished", { content: "" });
  await flush();
  assert.equal(first.calls.start.length, 2);
  assert.equal(second.calls.start.length, 1);
  second.event("stopped");
  const originalSession = second.state().activeSessionId;
  store.updateAiScopeState(second.scope, (state) => ({ ...state, activeSessionId: "other-session" }));
  second.sendNow();
  await flush();
  assert.equal(second.calls.start.length, 1);
  store.updateAiScopeState(second.scope, (state) => ({ ...state, activeSessionId: originalSession }));
  second.sendNow();
  await flush();
  assert.equal(second.calls.start[1].session_id, originalSession);
});

test("startup rejection restores a new conversation's text and attachments", async () => {
  const h = await harness();
  h.request.contexts = [{ id: "file", kind: "file", title: "demo.txt", content: "full text" }];
  h.handlers.start = async () => { throw new Error("invalid model"); };
  await h.start();
  assert.equal(h.state().activeSessionId, null);
  assert.equal(h.state().streamState, null);
  assert.equal(h.state().messages.length, 0);
  assert.equal(h.state().input, "first");
  assert.deepEqual(h.state().contextBlocks, h.request.contexts);
});

test("follow-ups queued during a failed first start return to the unsaved session and can be retried", async () => {
  const h = await harness();
  const acknowledgement = deferred();
  const normalStart = h.handlers.start;
  h.handlers.start = () => acknowledgement.promise;
  const pending = h.start();
  h.add("follow-up");
  assert.ok(h.state().queuedMessages[0].sessionId);
  acknowledgement.reject(new Error("invalid model"));
  await pending;
  assert.equal(h.state().activeSessionId, null);
  assert.equal(h.state().queuedMessages[0].sessionId, "");
  assert.equal(h.state().queuedMessages[0].request.session_id, null);
  h.handlers.start = normalStart;
  await h.sendNow();
  assert.equal(h.calls.start[1].content, "follow-up");
  assert.equal(h.state().queuedMessages.length, 0);
});

test("a failed initial send preserves its attachments separately from a newer draft", async () => {
  const h = await harness();
  const acknowledgement = deferred();
  h.request.contexts = [{ id: "old", kind: "file", title: "old.txt", content: "old text" }];
  h.handlers.start = () => acknowledgement.promise;
  const pending = h.start();
  const contexts = [{ id: "new", kind: "file", title: "new.txt", content: "new text" }];
  store.updateAiScopeState(h.scope, (state) => ({ ...state, input: "new draft", contextBlocks: contexts }));
  acknowledgement.reject(new Error("invalid model"));
  await pending;
  assert.equal(h.state().input, "new draft");
  assert.deepEqual(h.state().contextBlocks, contexts);
  assert.equal(h.state().queuedMessages[0].content, "first");
  assert.equal(h.state().queuedMessages[0].status, "failed");
  assert.deepEqual(h.state().queuedMessages[0].contexts, h.request.contexts);
});

test("a replayed insertion receipt cannot duplicate history or route output back to an older reply", async () => {
  const h = await harness();
  await h.start();
  const original = h.state().messages[1];
  const inject = (content) => {
    h.add(content);
    const item = h.state().queuedMessages[0];
    const previous = h.state().messages.at(-1);
    const injection = {
      queue_ids: [item.id], previous_message: { ...previous, status: "complete" },
      user_messages: [{ ...h.state().messages[0], id: item.id, content }],
      assistant_message: { ...previous, id: randomUUID(), content: "" },
    };
    h.event("message_injected", { injection });
    return injection;
  };
  const first = inject("insert one");
  const second = inject("insert two");
  h.event("message_injected", { message_id: original.id, injection: first });
  h.event("chunk", { delta: "latest reply" });
  assert.equal(h.state().messages.length, 6);
  assert.equal(h.state().streamState.assistantMessageId, second.assistant_message.id);
  assert.equal(h.state().messages.at(-1).content, "latest reply");
});

test("a lost insertion event is reconciled from persisted UUIDs without sending another turn", async () => {
  const h = await harness();
  await h.start();
  h.add("inserted but event lost");
  const item = h.state().queuedMessages[0];
  await h.sendNow();
  await flush();
  const previous = { ...h.state().messages[1], status: "complete" };
  const inserted = { ...h.state().messages[0], id: item.id, content: item.content };
  const reply = { ...previous, id: randomUUID(), content: "persisted reply" };
  h.handlers.get = async (id) => ({ id, messages: [h.state().messages[0], previous, inserted, reply] });
  h.event("finished", { message_id: reply.id, content: reply.content });
  await flush();
  assert.equal(h.calls.start.length, 1);
  assert.equal(h.calls.get.length, 1);
  assert.equal(h.state().queuedMessages.length, 0);
  assert.equal(h.state().messages.at(-1).content, "persisted reply");
});

test("failed reconciliation pauses sends and keeps uncertain messages immutable until retry", async () => {
  const h = await harness();
  await h.start();
  h.add("uncertain");
  const item = h.state().queuedMessages[0];
  await h.sendNow();
  await flush();
  const normalGet = h.handlers.get;
  h.handlers.get = async () => { throw new Error("database unavailable"); };
  h.event("finished", { content: "" });
  await flush();
  assert.equal(h.calls.start.length, 1);
  assert.equal(h.state().queueAutoRun, false);
  assert.equal(h.state().queuedMessages[0].status, "failed");
  assert.match(h.state().queuedMessages[0].error, /无法确认插入结果/);
  assert.equal(h.queue.canEditAiQueuedMessage(h.state().queuedMessages[0]), false);
  h.handlers.get = normalGet;
  await h.sendNow(item.id);
  assert.equal(h.calls.get.length, 2);
  assert.equal(h.calls.start[1].content, "uncertain");
});

test("reconciliation must not replace messages or resume sends in a different history session", async () => {
  const h = await harness();
  await h.start();
  h.add("pending confirmation");
  await h.sendNow();
  await flush();
  const originalSession = h.state().activeSessionId;
  const originalMessages = h.state().messages;
  const confirmation = deferred();
  h.handlers.get = () => confirmation.promise;
  h.event("finished", { content: "" });
  const otherMessages = [{ ...originalMessages[0], id: "other", session_id: "other-session", content: "other history" }];
  store.updateAiScopeState(h.scope, (state) => ({ ...state, activeSessionId: "other-session", messages: otherMessages }));
  confirmation.resolve({ id: originalSession, messages: originalMessages });
  await flush();
  assert.deepEqual(h.state().messages, otherMessages);
  assert.equal(h.state().queueAutoRun, false);
  assert.equal(h.calls.start.length, 1);
  assert.equal(h.state().queuedMessages[0].sessionId, originalSession);
});
