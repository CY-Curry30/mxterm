import { aiChatSessionGet, aiChatStreamEnqueue, aiChatStreamStart, aiChatStreamStop } from "../../shared/tauri/commands";
import { emitAiScopeEffect, readAiScopeState, updateAiScopeState, type AiQueuedMessage } from "./aiAssistantScopeStore";
import type { AiChatMessage, AiChatStreamStartRequest } from "./aiTypes";

const startingScopes = new Set<string>();
const insertingScopes = new Set<string>();
const reconcilingScopes = new Map<string, { sessionId: string; promise: Promise<boolean> }>();

export function canEditAiQueuedMessage(item: AiQueuedMessage): boolean {
  return item.status !== "sending" && item.status !== "accepted" && !item.immediate && !item.insertionStreamId;
}

function messageError(error: unknown): string {
  if (error && typeof error === "object" && "message" in error) {
    return String(error.message);
  }
  return error instanceof Error ? error.message : String(error);
}

function emptyMessage(id: string, sessionId: string, role: "user" | "assistant"): AiChatMessage {
  const now = Date.now().toString();
  return {
    id, session_id: sessionId, role, content: "", thinking: "", contexts: [],
    commands: [], tool_calls: [], status: role === "assistant" ? "streaming" : "complete",
    created_at: now, updated_at: now,
  };
}

// The store is updated before any IPC or SSH preparation. The same IDs cross
// the command boundary, so events arriving before the response are routable.
export async function startAiMessage(
  scopeKey: string,
  request: AiChatStreamStartRequest,
  ensureListener: () => Promise<void>,
  queuedId?: string,
): Promise<void> {
  if (readAiScopeState(scopeKey).streamState || startingScopes.has(scopeKey)) {
    // A stream can finish before the start IPC acknowledges it. Preserve a
    // follow-up clicked in that window instead of dropping the cleared draft.
    if (!queuedId) queueAiMessage(scopeKey, request);
    return;
  }
  if (reconcilingScopes.has(scopeKey)) {
    if (!queuedId) queueAiMessage(scopeKey, request);
    return;
  }
  const sessionId = request.session_id || crypto.randomUUID();
  const ids = {
    session_id: sessionId,
    stream_id: crypto.randomUUID(),
    user_message_id: crypto.randomUUID(),
    assistant_message_id: crypto.randomUUID(),
  };
  const user = { ...emptyMessage(ids.user_message_id, sessionId, "user"), content: request.content, contexts: request.contexts || [] };
  const assistant = emptyMessage(ids.assistant_message_id, sessionId, "assistant");
  startingScopes.add(scopeKey);
  let started = false;
  updateAiScopeState(scopeKey, (state) => ({
    ...state, activeSessionId: sessionId, messages: [...state.messages, user, assistant],
    queueAutoRun: queuedId ? state.queueAutoRun : false,
    streamState: { sessionId, streamId: ids.stream_id, assistantMessageId: assistant.id, ready: false, request },
    queuedMessages: state.queuedMessages.map((item) => {
      const scoped = !request.session_id && item.sessionId === ""
        ? { ...item, sessionId, request: { ...item.request, session_id: sessionId } } : item;
      return scoped.id === queuedId ? { ...scoped, status: "sending" } : scoped;
    }),
  }));
  try {
    await ensureListener();
    const response = await aiChatStreamStart({ ...request, client_ids: ids });
    started = true;
    updateAiScopeState(scopeKey, (state) => ({
      ...state,
      messages: state.messages.map((message) => message.id === ids.user_message_id ? response.user_message : message),
      streamState: state.streamState?.streamId === ids.stream_id
        ? { ...state.streamState, ready: true, request: { ...request, content: "", contexts: [] } } : state.streamState,
      queuedMessages: state.queuedMessages.filter((item) => item.id !== queuedId),
    }));
    if (readAiScopeState(scopeKey).streamState?.stopRequested) {
      await aiChatStreamStop(ids.stream_id);
    }
  } catch (error) {
    const text = messageError(error);
    if (started) {
      updateAiScopeState(scopeKey, (state) => ({ ...state,
        streamState: state.streamState?.streamId === ids.stream_id ? { ...state.streamState, stopRequested: false } : state.streamState,
      }));
      emitAiScopeEffect(scopeKey, { kind: "stream_ended", status: "error", error: text });
      return;
    }
    updateAiScopeState(scopeKey, (state) => {
      // A start rejection has no persisted message. Restore a new-session draft
      // and keep a failed queued send available to retry without duplicating it.
      if (state.streamState?.streamId !== ids.stream_id) return state;
      const restoreDraft = !queuedId && !state.input && state.contextBlocks.length === 0;
      const retained: AiQueuedMessage[] = !queuedId && !restoreDraft ? [{
        id: crypto.randomUUID(), sessionId: request.session_id || "", content: request.content,
        contexts: request.contexts || [], request, immediate: false, status: "failed", error: text,
      }] : [];
      return {
        ...state, activeSessionId: request.session_id || null, streamState: null, queueAutoRun: false,
        messages: state.messages.filter((message) => message.id !== user.id && message.id !== assistant.id),
        input: restoreDraft ? request.content : state.input,
        contextBlocks: restoreDraft ? request.contexts || [] : state.contextBlocks,
        queuedMessages: [...retained, ...state.queuedMessages.map((item) => {
          const scoped = !request.session_id && item.sessionId === sessionId
            ? { ...item, sessionId: "", request: { ...item.request, session_id: null } } : item;
          return scoped.id === queuedId ? { ...scoped, status: "failed" as const, immediate: false, error: text } : scoped;
        })],
      };
    });
    emitAiScopeEffect(scopeKey, { kind: "stream_ended", status: "error", error: text });
  } finally {
    startingScopes.delete(scopeKey);
    void drainAiMessageQueue(scopeKey, ensureListener);
  }
}

export function queueAiMessage(scopeKey: string, request: AiChatStreamStartRequest) {
  const state = readAiScopeState(scopeKey);
  const sessionId = state.streamState?.sessionId || state.activeSessionId;
  const item: AiQueuedMessage = {
    id: crypto.randomUUID(), sessionId: sessionId || "", content: request.content, contexts: request.contexts || [],
    request: { ...request, session_id: sessionId }, status: "queued", immediate: false,
  };
  updateAiScopeState(scopeKey, (current) => ({ ...current, queuedMessages: [...current.queuedMessages, item] }));
  return item.id;
}

export async function sendQueuedAiMessageNow(scopeKey: string, queueId: string, ensureListener: () => Promise<void>) {
  const state = readAiScopeState(scopeKey);
  const item = state.queuedMessages.find((item) => item.id === queueId && item.sessionId === (state.activeSessionId || ""));
  if (!item || item.status === "sending" || item.status === "accepted") return;
  if (!state.streamState) {
    if (item.insertionStreamId || reconcilingScopes.has(scopeKey)) {
      if (!await reconcileAiMessageQueue(scopeKey, item.sessionId)) return;
      if (!readAiScopeState(scopeKey).queuedMessages.some((queued) => queued.id === item.id)) return;
      if (readAiScopeState(scopeKey).activeSessionId !== state.activeSessionId) return;
    }
    const currentItem = readAiScopeState(scopeKey).queuedMessages.find((queued) => queued.id === item.id);
    if (!currentItem) return;
    await startAiMessage(scopeKey, { ...currentItem.request, content: currentItem.content, contexts: currentItem.contexts }, ensureListener, item.id);
    return;
  }
  if (!state.streamState.request?.agent || state.streamState.stopRequested) return;
  updateAiScopeState(scopeKey, (current) => ({
    ...current, queuedMessages: current.queuedMessages.map((queued) => queued.id === queueId
      ? { ...queued, immediate: true, status: "queued", error: undefined } : queued),
  }));
  void drainAiMessageQueue(scopeKey, ensureListener);
}

export async function stopAiMessage(scopeKey: string) {
  const stream = readAiScopeState(scopeKey).streamState;
  if (!stream || stream.stopRequested) return;
  updateAiScopeState(scopeKey, (state) => ({
    ...state, queueAutoRun: false,
    streamState: state.streamState ? { ...state.streamState, stopRequested: true } : null,
  }));
  if (!stream.ready) return; // Start acknowledgement sends the stop request.
  try {
    await aiChatStreamStop(stream.streamId);
  } catch (error) {
    updateAiScopeState(scopeKey, (state) => ({ ...state,
      streamState: state.streamState?.streamId === stream.streamId ? { ...state.streamState, stopRequested: false } : state.streamState,
    }));
    throw error;
  }
}

export function finishAiMessageQueue(scopeKey: string, status: "finished" | "stopped" | "error", ensureListener: () => Promise<void>) {
  const sessionId = readAiScopeState(scopeKey).activeSessionId;
  const unresolved = readAiScopeState(scopeKey).queuedMessages.some((item) => item.sessionId === sessionId && item.insertionStreamId);
  updateAiScopeState(scopeKey, (state) => ({
    ...state, queueAutoRun: !unresolved && status === "finished",
    queuedMessages: state.queuedMessages.map((item) => item.sessionId === state.activeSessionId && item.status !== "failed"
      ? { ...item, status: "queued", immediate: false } : item),
  }));
  if (unresolved && sessionId) {
    // A missing event/IPC acknowledgement is not evidence that insertion
    // failed. Confirm persisted user IDs before allowing another send.
    void reconcileAiMessageQueue(scopeKey, sessionId).then((success) => {
      if (!success) return;
      updateAiScopeState(scopeKey, (state) => state.activeSessionId === sessionId && !state.streamState
        ? { ...state, queueAutoRun: status === "finished" } : state);
      void drainAiMessageQueue(scopeKey, ensureListener);
    });
    return;
  }
  void drainAiMessageQueue(scopeKey, ensureListener);
}

function reconcileAiMessageQueue(scopeKey: string, sessionId: string): Promise<boolean> {
  const pending = reconcilingScopes.get(scopeKey);
  if (pending) {
    return pending.sessionId === sessionId ? pending.promise
      : pending.promise.then(() => reconcileAiMessageQueue(scopeKey, sessionId));
  }
  updateAiScopeState(scopeKey, (state) => ({ ...state,
    queuedMessages: state.queuedMessages.map((item) => item.sessionId === sessionId && item.insertionStreamId
      ? { ...item, status: "sending" } : item),
  }));
  const reconciliation = (async () => {
    try {
      const session = await aiChatSessionGet(sessionId);
      const committedIds = new Set(session.messages.filter((message) => message.role === "user").map((message) => message.id));
      updateAiScopeState(scopeKey, (state) => ({ ...state,
        // The database is authoritative after the run ends. Preserve the
        // in-memory reasoning positions for messages whose events did arrive.
        messages: state.activeSessionId === sessionId && !state.streamState ? session.messages.map((message) => ({
          ...message, thinking_blocks: state.messages.find((item) => item.id === message.id)?.thinking_blocks,
        })) : state.messages,
        queuedMessages: state.queuedMessages.filter((item) => item.sessionId !== sessionId || !committedIds.has(item.id))
          .map((item) => item.sessionId === sessionId && item.insertionStreamId
            ? { ...item, insertionStreamId: undefined, immediate: false, status: "queued", error: undefined } : item),
      }));
      return true;
    } catch (error) {
      updateAiScopeState(scopeKey, (state) => ({ ...state,
        queueAutoRun: state.activeSessionId === sessionId ? false : state.queueAutoRun,
        queuedMessages: state.queuedMessages.map((item) => item.sessionId === sessionId && item.insertionStreamId
          ? { ...item, status: "failed", immediate: false, error: `无法确认插入结果，请重试：${messageError(error)}` } : item),
      }));
      return false;
    } finally {
      reconcilingScopes.delete(scopeKey);
    }
  })();
  reconcilingScopes.set(scopeKey, { sessionId, promise: reconciliation });
  return reconciliation;
}

async function drainAiMessageQueue(scopeKey: string, ensureListener: () => Promise<void>): Promise<void> {
  if (startingScopes.has(scopeKey) || insertingScopes.has(scopeKey) || reconcilingScopes.has(scopeKey)) return;
  const state = readAiScopeState(scopeKey);
  if (!state.streamState) {
    if (!state.queueAutoRun) return;
    const item = state.queuedMessages.find((item) => item.sessionId === state.activeSessionId);
    // A rejected message requires an explicit retry; do not skip past it.
    if (!item || item.status !== "queued") return;
    await startAiMessage(scopeKey, { ...item.request, content: item.content, contexts: item.contexts }, ensureListener, item.id);
    return;
  }
  const stream = state.streamState;
  if (!stream.ready || stream.stopRequested) return;
  const item = state.queuedMessages.find((item) => item.sessionId === stream.sessionId && item.immediate && item.status === "queued");
  if (!item) return;
  insertingScopes.add(scopeKey);
  updateAiScopeState(scopeKey, (current) => ({ ...current,
    queuedMessages: current.queuedMessages.map((queued) => queued.id === item.id ? { ...queued, status: "sending", insertionStreamId: stream.streamId } : queued),
  }));
  try {
    await aiChatStreamEnqueue({ stream_id: stream.streamId, queue_id: item.id, content: item.content, contexts: item.contexts });
    updateAiScopeState(scopeKey, (current) => current.streamState?.streamId !== stream.streamId ? current : ({ ...current,
      queuedMessages: current.queuedMessages.map((queued) => queued.id === item.id ? { ...queued, status: "accepted" } : queued),
    }));
  } catch (error) {
    const closed = error && typeof error === "object" && "code" in error && error.code === "ai_injection_closed";
    updateAiScopeState(scopeKey, (current) => current.streamState?.streamId !== stream.streamId ? current : ({ ...current,
      queuedMessages: current.queuedMessages.map((queued) => queued.id === item.id
        ? { ...queued, status: closed ? "queued" : "failed", immediate: false, error: closed ? undefined : messageError(error) } : queued),
    }));
  } finally {
    insertingScopes.delete(scopeKey);
    void drainAiMessageQueue(scopeKey, ensureListener);
  }
}
