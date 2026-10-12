import { Check, ListPlus, Pencil, Send, Trash2, X, Zap } from "lucide-react";
import { useState } from "react";
import { Tooltip } from "../../shared/ui/Tooltip";
import type { AiQueuedMessage } from "./aiAssistantScopeStore";
import { canEditAiQueuedMessage } from "./aiMessageQueue";

interface AiQueuedMessagesProps {
  items: AiQueuedMessage[];
  running: boolean;
  canInsert: boolean;
  onSend: (id: string) => void;
  onDelete: (id: string) => void;
  onEdit: (id: string, content: string) => void;
}

export function AiQueuedMessages({ items, running, canInsert, onSend, onDelete, onEdit }: AiQueuedMessagesProps) {
  const [editingId, setEditingId] = useState<string | null>(null);
  const [draft, setDraft] = useState("");
  if (!items.length) return null;
  return (
    <section className="ai-message-queue" aria-label="待发送消息">
      <div className="ai-message-queue-heading" role="status">
        <ListPlus className="ui-icon" aria-hidden="true" />
        <span>{items.length} 条待发送{running && items.every((item) => !item.immediate && item.status !== "accepted" && item.status !== "sending") ? " · 本轮结束后发送" : ""}</span>
      </div>
      <div className="ai-message-queue-list">
        {items.map((item) => {
          const busy = item.status === "sending" || item.status === "accepted";
          const canEdit = canEditAiQueuedMessage(item);
          const editing = editingId === item.id;
          return (
            <article className="ai-message-queue-item" key={item.id}>
              <div className="ai-message-queue-body">
                {editing ? (
                  <textarea
                    className="ai-message-queue-editor"
                    aria-label="编辑待发送消息"
                    value={draft}
                    autoFocus
                    onChange={(event) => setDraft(event.currentTarget.value)}
                    onKeyDown={(event) => {
                      if (event.key === "Escape") { event.stopPropagation(); setEditingId(null); }
                      if (event.key === "Enter" && (event.ctrlKey || event.metaKey) && !event.nativeEvent.isComposing) {
                        event.preventDefault(); event.stopPropagation();
                        if (canEdit && (draft.trim() || item.contexts.length)) { onEdit(item.id, draft.trim()); setEditingId(null); }
                      }
                    }}
                  />
                ) : <p title={item.content}>{item.content || "附件消息"}</p>}
                {item.contexts.length > 0 ? <small title={item.contexts.map((block) => block.title).join("、")}>{item.contexts.length} 个附件 / 上下文</small> : null}
                {busy || item.immediate ? <small role="status">{item.status === "sending" ? item.insertionStreamId && !running ? "确认插入结果…" : "发送中…" : "正在接管当前执行…"}</small> : null}
                {item.error ? <small className="ai-message-queue-error" role="alert">{item.error}</small> : null}
              </div>
              <div className="ai-message-queue-actions">
                {editing ? (
                  <>
                    <Tooltip label="保存修改（Ctrl / Cmd + Enter）">
                      <button className="ai-mini-button" type="button" aria-label="保存队列消息" disabled={!canEdit || !draft.trim() && !item.contexts.length} onClick={() => { onEdit(item.id, draft.trim()); setEditingId(null); }}>
                        <Check className="ui-icon" aria-hidden="true" />
                      </button>
                    </Tooltip>
                    <Tooltip label="取消编辑">
                      <button className="ai-mini-button" type="button" aria-label="取消编辑" onClick={() => setEditingId(null)}><X className="ui-icon" aria-hidden="true" /></button>
                    </Tooltip>
                  </>
                ) : (
                  <>
                    <Tooltip label={running ? canInsert ? "取消当前执行并立即插入" : "本轮结束后发送" : item.insertionStreamId ? "确认插入结果并重试" : "发送此消息"}>
                      <button className="ai-mini-button ai-message-queue-send" type="button" aria-label={running ? "立即插入" : "发送队列消息"} disabled={busy || !canInsert} onClick={() => onSend(item.id)}>
                        {running ? <Zap className="ui-icon" aria-hidden="true" /> : <Send className="ui-icon" aria-hidden="true" />}<span>{item.status === "failed" ? "重试" : running ? "立即" : "发送"}</span>
                      </button>
                    </Tooltip>
                    <Tooltip label="编辑">
                      <button className="ai-mini-button" type="button" aria-label="编辑队列消息" disabled={!canEdit} onClick={() => { setDraft(item.content); setEditingId(item.id); }}><Pencil className="ui-icon" aria-hidden="true" /></button>
                    </Tooltip>
                    <Tooltip label="删除">
                      <button className="ai-mini-button" type="button" aria-label="删除队列消息" disabled={!canEdit} onClick={() => onDelete(item.id)}><Trash2 className="ui-icon" aria-hidden="true" /></button>
                    </Tooltip>
                  </>
                )}
              </div>
            </article>
          );
        })}
      </div>
    </section>
  );
}
