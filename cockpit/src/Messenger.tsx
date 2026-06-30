// Messenger + whiteboard panel (P10.3, #115). A live chat (sent via the P10.1
// routes, streamed back over a Tauri Channel), a collaborative whiteboard that
// rides the same message stream (strokes = messages scoped to the `whiteboard`
// group), notification popups on incoming messages, and a simple task list.
// Identity stays host-authoritative (ADR-0016): the webview sends only content +
// scope; the host stamps the `actor`.
import { useEffect, useRef, useState } from "react";
import {
  me,
  messageLog,
  sendMessage,
  stopMessages,
  watchMessages,
  type MessagesEvent,
  type MessengerMessage,
  type MessengerScope,
} from "./api";
import Whiteboard, { type Stroke } from "./Whiteboard";
import { useToast } from "./Toast";

/** The group whose messages carry whiteboard strokes rather than chat text. */
const WHITEBOARD_GROUP = "whiteboard";

type Task = { id: number; text: string; done: boolean };

const isWhiteboard = (scope: MessengerScope): boolean =>
  scope.kind === "group" && scope.name === WHITEBOARD_GROUP;

/** Parse a whiteboard message's text into a stroke, or `null` if it isn't one. */
function parseStroke(text: string): Stroke | null {
  try {
    const v: unknown = JSON.parse(text);
    if (
      typeof v === "object" &&
      v !== null &&
      "points" in v &&
      Array.isArray((v as { points: unknown }).points)
    ) {
      return v as Stroke;
    }
  } catch {
    // Not a stroke payload — ignore.
  }
  return null;
}

const shortActor = (email: string): string => email.split("@")[0];

export default function Messenger() {
  const toast = useToast();
  const [messages, setMessages] = useState<MessengerMessage[]>([]);
  const [self, setSelf] = useState<string>("");
  const [text, setText] = useState("");
  const [scopeKind, setScopeKind] = useState<"public" | "group">("public");
  const [groupName, setGroupName] = useState("general");
  const [tasks, setTasks] = useState<Task[]>([]);
  const [newTask, setNewTask] = useState("");

  const seenChat = useRef<number>(-1);
  const taskId = useRef(0);

  // Self identity (host-side) for own-message styling + notification filtering.
  useEffect(() => {
    let active = true;
    me()
      .then((id) => active && setSelf(id.email))
      .catch(() => {});
    return () => {
      active = false;
    };
  }, []);

  // Instant initial log, then live snapshots over the Channel (stopped on unmount).
  useEffect(() => {
    let active = true;
    let watchId: string | null = null;
    messageLog()
      .then((m) => active && setMessages(m))
      .catch(() => {});
    watchMessages((event: MessagesEvent) => {
      if (!active) return;
      switch (event.event) {
        case "snapshot":
          setMessages(event.messages);
          break;
        case "error":
          toast.error(event.message);
          break;
        case "stopped":
          break;
      }
    })
      .then((id) => {
        watchId = id;
        if (!active) void stopMessages(id);
      })
      .catch((e) => active && toast.error(String(e)));
    return () => {
      active = false;
      if (watchId) void stopMessages(watchId);
    };
  }, []);

  // Fire a notification popup when a NEW chat message arrives from someone else.
  useEffect(() => {
    const chat = messages.filter((m) => !isWhiteboard(m.scope));
    if (seenChat.current < 0) {
      seenChat.current = chat.length; // first load — don't notify on history
      return;
    }
    if (chat.length > seenChat.current) {
      // Only notify once our own identity is known, so a fast initial snapshot
      // can't toast on our own message before `me()` resolves.
      const fresh =
        self === ""
          ? []
          : chat.slice(seenChat.current).filter((m) => m.actor !== self);
      seenChat.current = chat.length;
      if (fresh.length > 0) {
        const last = fresh[fresh.length - 1];
        toast.info(`${shortActor(last.actor)}: ${last.text}`);
      }
    }
  }, [messages, self, toast]);

  const chat = messages.filter((m) => !isWhiteboard(m.scope));
  const strokes = messages
    .filter((m) => isWhiteboard(m.scope))
    .map((m) => parseStroke(m.text))
    .filter((s): s is Stroke => s !== null);

  const send = async () => {
    const body = text.trim();
    if (!body) return;
    const scope: MessengerScope =
      scopeKind === "group"
        ? { kind: "group", name: groupName.trim() || "general" }
        : { kind: "public" };
    try {
      await sendMessage(body, scope);
      setText("");
    } catch (e) {
      toast.error(String(e));
    }
  };

  const sendStroke = (stroke: Stroke) => {
    sendMessage(JSON.stringify(stroke), {
      kind: "group",
      name: WHITEBOARD_GROUP,
    }).catch((e) => toast.error(String(e)));
  };

  const addTask = () => {
    const body = newTask.trim();
    if (!body) return;
    taskId.current += 1;
    setTasks((ts) => [...ts, { id: taskId.current, text: body, done: false }]);
    setNewTask("");
  };

  const toggleTask = (id: number) =>
    setTasks((ts) =>
      ts.map((t) => (t.id === id ? { ...t, done: !t.done } : t)),
    );

  const removeTask = (id: number) =>
    setTasks((ts) => ts.filter((t) => t.id !== id));

  const scopeChip = (scope: MessengerScope): string =>
    scope.kind === "group" ? `group:${scope.name}` : "public";

  return (
    <section className="card messenger">
      <h1>Messenger</h1>
      <p className="subtitle">Chat, whiteboard, and tasks over the firm bus</p>

      <div className="messenger-grid">
        <div className="panel msg-chat">
          <h2>Messages</h2>
          <div className="msg-list">
            {chat.length === 0 ? (
              <p className="hint">No messages yet — say hello.</p>
            ) : (
              chat.map((m, i) => (
                <div
                  className={`msg ${m.actor === self ? "is-self" : ""}`}
                  key={i}
                >
                  <div className="msg-meta">
                    <span className="msg-actor">{shortActor(m.actor)}</span>
                    {m.scope.kind === "group" && (
                      <span className="chip">{scopeChip(m.scope)}</span>
                    )}
                  </div>
                  <div className="msg-text">{m.text}</div>
                </div>
              ))
            )}
          </div>
          <div className="msg-compose">
            <div className="msg-scope">
              <select
                value={scopeKind}
                onChange={(e) =>
                  setScopeKind(e.target.value === "group" ? "group" : "public")
                }
              >
                <option value="public">public</option>
                <option value="group">group</option>
              </select>
              {scopeKind === "group" && (
                <input
                  value={groupName}
                  onChange={(e) => setGroupName(e.target.value)}
                  placeholder="group name"
                />
              )}
            </div>
            <textarea
              value={text}
              onChange={(e) => setText(e.target.value)}
              rows={2}
              placeholder="Message…"
              onKeyDown={(e) => {
                if (e.key === "Enter" && !e.shiftKey) {
                  e.preventDefault();
                  void send();
                }
              }}
            />
            <button className="btn-primary" onClick={send} disabled={!text.trim()}>
              Send
            </button>
          </div>
        </div>

        <div className="panel msg-board">
          <h2>Whiteboard</h2>
          <p className="hint">Draw to broadcast strokes on the bus.</p>
          <Whiteboard strokes={strokes} onStroke={sendStroke} />
        </div>

        <div className="panel msg-tasks">
          <h2>Tasks</h2>
          <div className="task-add">
            <input
              value={newTask}
              onChange={(e) => setNewTask(e.target.value)}
              placeholder="New task…"
              onKeyDown={(e) => e.key === "Enter" && addTask()}
            />
            <button className="btn-secondary" onClick={addTask} disabled={!newTask.trim()}>
              Add
            </button>
          </div>
          {tasks.length === 0 ? (
            <p className="hint">No tasks.</p>
          ) : (
            <ul className="task-list">
              {tasks.map((t) => (
                <li key={t.id} className={t.done ? "is-done" : ""}>
                  <label>
                    <input
                      type="checkbox"
                      checked={t.done}
                      onChange={() => toggleTask(t.id)}
                    />
                    <span>{t.text}</span>
                  </label>
                  <button
                    className="link-btn"
                    onClick={() => removeTask(t.id)}
                    aria-label="remove task"
                  >
                    ×
                  </button>
                </li>
              ))}
            </ul>
          )}
        </div>
      </div>
    </section>
  );
}
