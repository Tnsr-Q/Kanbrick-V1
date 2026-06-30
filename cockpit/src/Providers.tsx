// BYO-AI console (P9.4, #104): pick a provider/model, save a key host-side, and
// stream a completion token-by-token over the Tauri Channel. The webview holds no
// key — it sends only { provider, model, prompt } and the host injects the secret.
import { useEffect, useState } from "react";
import {
  cancelCompletion,
  listProviderKeys,
  saveProviderKey,
  streamCompletion,
  type KeyMetadata,
  type ProviderKind,
  type StreamEvent,
  type Usage,
} from "./api";
import { useToast } from "./Toast";

const PROVIDERS: ProviderKind[] = ["anthropic", "openai", "cerebras"];

const DEFAULT_MODEL: Record<ProviderKind, string> = {
  anthropic: "claude-opus-4-8",
  openai: "gpt-4o",
  cerebras: "llama-3.3-70b",
};

export default function Providers() {
  const toast = useToast();
  const [keys, setKeys] = useState<KeyMetadata[]>([]);
  const [provider, setProvider] = useState<ProviderKind>("anthropic");
  const [model, setModel] = useState<string>(DEFAULT_MODEL.anthropic);
  const [label, setLabel] = useState("");
  const [secret, setSecret] = useState("");
  const [prompt, setPrompt] = useState("");
  const [response, setResponse] = useState("");
  const [usage, setUsage] = useState<Usage | null>(null);
  const [status, setStatus] = useState<string | null>(null);
  const [streaming, setStreaming] = useState(false);
  const [streamId, setStreamId] = useState<string | null>(null);

  const refreshKeys = () =>
    listProviderKeys()
      .then(setKeys)
      .catch((e) => toast.error(String(e)));

  useEffect(() => {
    void refreshKeys();
  }, []);

  const pickProvider = (p: ProviderKind) => {
    setProvider(p);
    setModel(DEFAULT_MODEL[p]);
  };

  const save = async () => {
    try {
      await saveProviderKey(provider, label || `${provider} key`, secret);
      setSecret("");
      setLabel("");
      await refreshKeys();
      toast.success(`Saved ${provider} key.`);
    } catch (e) {
      toast.error(String(e));
    }
  };

  const send = async () => {
    setResponse("");
    setUsage(null);
    setStatus(null);
    setStreaming(true);
    try {
      const id = await streamCompletion(
        { provider, model, prompt },
        (event: StreamEvent) => {
          switch (event.event) {
            case "delta":
              setResponse((r) => r + event.text);
              break;
            case "done":
              setUsage(event.usage);
              setStatus("done");
              setStreaming(false);
              break;
            case "error":
              toast.error(event.message);
              setStreaming(false);
              break;
            case "cancelled":
              setStatus("cancelled");
              setStreaming(false);
              break;
          }
        },
      );
      setStreamId(id);
    } catch (e) {
      toast.error(String(e));
      setStreaming(false);
    }
  };

  const cancel = async () => {
    if (streamId) await cancelCompletion(streamId);
  };

  const hasKey = keys.some((k) => k.provider === provider);

  return (
    <section className="card providers">
      <h1>BYO-AI</h1>
      <p className="subtitle">
        Stream a completion — your key is injected host-side (P9.4)
      </p>

      <div className="panel">
        <h2>Your keys</h2>
        {keys.length === 0 ? (
          <p className="hint">No keys saved yet — add one below.</p>
        ) : (
          <div className="chips">
            {keys.map((k) => (
              <span className="chip" key={k.id}>
                {k.provider} · {k.label}
              </span>
            ))}
          </div>
        )}
        <div className="field">
          <label>Provider</label>
          <select
            value={provider}
            onChange={(e) => pickProvider(e.target.value as ProviderKind)}
          >
            {PROVIDERS.map((p) => (
              <option key={p} value={p}>
                {p}
              </option>
            ))}
          </select>
        </div>
        <div className="field">
          <label>Label</label>
          <input
            value={label}
            onChange={(e) => setLabel(e.target.value)}
            placeholder="personal"
          />
        </div>
        <div className="field">
          <label>API key</label>
          <input
            type="password"
            value={secret}
            onChange={(e) => setSecret(e.target.value)}
            placeholder="sk-…"
          />
        </div>
        <button className="btn-secondary" onClick={save} disabled={!secret}>
          Save key
        </button>
      </div>

      <div className="panel">
        <h2>Prompt</h2>
        <div className="field">
          <label>Model</label>
          <input value={model} onChange={(e) => setModel(e.target.value)} />
        </div>
        <div className="field">
          <label>Prompt</label>
          <textarea
            value={prompt}
            onChange={(e) => setPrompt(e.target.value)}
            rows={3}
            placeholder="Ask something…"
          />
        </div>
        {!hasKey && (
          <p className="hint">
            No key saved for {provider} — save one above first.
          </p>
        )}
        <div className="providers-actions">
          {streaming ? (
            <button className="btn-secondary" onClick={cancel}>
              Cancel
            </button>
          ) : (
            <button
              className="btn-primary"
              onClick={send}
              disabled={!prompt || !hasKey}
            >
              Send
            </button>
          )}
        </div>
      </div>

      {(response || status) && (
        <div className="panel">
          <h2>
            Response{" "}
            {status && <span className="chip">{status}</span>}
          </h2>
          <pre className="providers-response">{response}</pre>
          {usage && (
            <p className="hint">
              tokens — in {usage.input} · out {usage.output}
              {usage.reasoning > 0 && ` · reasoning ${usage.reasoning}`}
            </p>
          )}
        </div>
      )}
    </section>
  );
}
