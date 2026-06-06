import { useEffect, useMemo, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import {
  Archive,
  CheckCircle2,
  DatabaseZap,
  FolderOpen,
  KeyRound,
  Loader2,
  RefreshCcw,
  RotateCcw,
  ShieldCheck,
  TerminalSquare,
  Wrench
} from "lucide-react";

const isTauriRuntime = () => Boolean(window.__TAURI_INTERNALS__);

const previewState = {
  codexHome: "Preview Mode · Tauri 后端运行时读取真实 ~/.codex",
  configPath: "",
  authPath: "",
  configExists: true,
  authExists: true,
  backupRoot: "",
  backupDirs: [],
  config: {
    rootProvider: "simplaj",
    providers: [
      {
        id: "simplaj",
        name: "simplaj",
        baseUrl: "https://api.example.local/v1",
        wireApi: "responses",
        requiresOpenaiAuth: "true",
        hasExperimentalBearerToken: true,
        experimentalBearerTokenMasked: "sk-...demo"
      }
    ]
  },
  runtime: {
    nodeVersion: "not required",
    nodeOkForProviderSync: true,
    npxVersion: "not required",
    syncPackage: "native-rust-rusqlite",
    syncEngine: "native-rust-rusqlite",
    nodeRequiredForSync: false
  }
};

async function callTauri(command, args) {
  if (isTauriRuntime()) {
    return invoke(command, args);
  }
  if (command === "inspect") return previewState;
  if (command === "list_auth_token_candidates") return { candidates: [] };
  if (command === "run_provider_sync") {
    return {
      ok: true,
      code: 0,
      stdout: "Preview mode: Tauri 桌面端会使用 Rust 原生同步引擎。",
      stderr: "",
      command: "native-rust-rusqlite preview",
      durationMs: 0,
      nodeRequirement: ""
    };
  }
  if (command === "repair_provider") {
    return {
      changed: true,
      providerId: args?.customName || "simplaj",
      backupDir: "preview",
      message: "Preview mode: 桌面端会备份并修改真实 config.toml。",
      sync: null
    };
  }
  if (command === "backup_remove_auth") {
    return { removed: true, authPath: "preview", backupDir: "preview", message: "Preview mode: 桌面端会备份并移除 auth.json。" };
  }
  if (command === "apply_experimental_token") {
    return { applied: true, providerId: args?.providerId || "simplaj", tokenMasked: "sk-...demo", backupDir: "preview" };
  }
  return { opened: false };
}

const api = {
  inspect: () => callTauri("inspect"),
  repairProvider: ({ customName, runSync }) => callTauri("repair_provider", { customName, runSync }),
  runProviderSync: ({ command, providerId } = {}) => callTauri("run_provider_sync", { command, providerId }),
  backupAndRemoveAuth: () => callTauri("backup_remove_auth"),
  listAuthTokenCandidates: () => callTauri("list_auth_token_candidates"),
  applyExperimentalToken: ({ providerId, candidateId, token }) => callTauri("apply_experimental_token", {
    providerId,
    candidateId,
    token
  }),
  openPath: ({ targetPath }) => callTauri("open_path", { targetPath })
};

function formatOutput(result) {
  if (!result) return "";
  const parts = [];
  if (result.command) parts.push(`$ ${result.command}`);
  if (result.stdout) parts.push(result.stdout.trim());
  if (result.stderr) parts.push(result.stderr.trim());
  if (result.nodeRequirement && !result.ok) parts.push(result.nodeRequirement);
  return parts.filter(Boolean).join("\n\n");
}

function ProviderTable({ providers = [], rootProvider }) {
  if (!providers.length) {
    return <div className="empty-line">未读取到 model_providers 配置。</div>;
  }
  return (
    <div className="provider-table">
      <div className="provider-row provider-row-head">
        <span>ID</span>
        <span>Name</span>
        <span>Wire</span>
        <span>Token</span>
      </div>
      {providers.map((provider) => (
        <div className="provider-row" key={provider.id}>
          <span className="provider-id">
            {provider.id}
            {provider.id === rootProvider ? <b>当前</b> : null}
          </span>
          <span>{provider.name || "-"}</span>
          <span>{provider.wireApi || "-"}</span>
          <span>{provider.hasExperimentalBearerToken ? provider.experimentalBearerTokenMasked : "未写入"}</span>
        </div>
      ))}
    </div>
  );
}

function StatusPill({ tone = "neutral", children }) {
  return <span className={`status-pill ${tone}`}>{children}</span>;
}

function LogPanel({ logs, onClear }) {
  return (
    <section className="console-panel">
      <div className="section-title">
        <TerminalSquare size={18} />
        <span>操作日志</span>
        <button className="ghost-button compact" onClick={onClear}>清空</button>
      </div>
      <pre>{logs.length ? logs.join("\n\n") : "等待操作..."}</pre>
    </section>
  );
}

export default function App() {
  const [state, setState] = useState(null);
  const [customName, setCustomName] = useState("simplaj");
  const [selectedProvider, setSelectedProvider] = useState("");
  const [manualToken, setManualToken] = useState("");
  const [selectedCandidate, setSelectedCandidate] = useState("");
  const [tokenCandidates, setTokenCandidates] = useState([]);
  const [logs, setLogs] = useState([]);
  const [busy, setBusy] = useState("");

  const providers = state?.config?.providers || [];
  const rootProvider = state?.config?.rootProvider || "";
  const openAIProvider = useMemo(() => (
    providers.find((provider) => (
      provider.id.toLowerCase() === "openai" || String(provider.name).toLowerCase() === "openai"
    ))
  ), [providers]);
  const selectedProviderValue = selectedProvider || rootProvider || providers[0]?.id || "simplaj";

  async function refresh() {
    const nextState = await api.inspect();
    setState(nextState);
    if (!selectedProvider && nextState?.config?.rootProvider) {
      setSelectedProvider(nextState.config.rootProvider);
    }
  }

  useEffect(() => {
    refresh().catch((error) => pushLog(`刷新失败：${error.message}`));
  }, []);

  function pushLog(message) {
    setLogs((current) => [`[${new Date().toLocaleTimeString()}] ${message}`, ...current].slice(0, 20));
  }

  async function runAction(label, action) {
    setBusy(label);
    try {
      const result = await action();
      pushLog(`${label}\n${typeof result === "string" ? result : JSON.stringify(result, null, 2)}`);
      await refresh();
      return result;
    } catch (error) {
      pushLog(`${label} 失败\n${error.message}`);
      return null;
    } finally {
      setBusy("");
    }
  }

  async function loadTokenCandidates() {
    return runAction("读取 auth 备份 token", async () => {
      const result = await api.listAuthTokenCandidates();
      setTokenCandidates(result.candidates || []);
      if (result.candidates?.[0]) setSelectedCandidate(result.candidates[0].id);
      return result.candidates?.length
        ? `找到 ${result.candidates.length} 个 sk- token 候选。`
        : "没有在 auth.json 备份中找到 sk- token。";
    });
  }

  const runtimeTone = state?.runtime?.nodeRequiredForSync ? "warn" : "ok";
  const canUseConfig = Boolean(state?.configExists);
  const isBusy = Boolean(busy);

  return (
    <main className="app-shell">
      <header className="topbar">
        <div className="brand-block">
          <div className="brand-mark"><Wrench size={22} /></div>
          <div>
            <h1>Codex API 工具箱</h1>
            <p>{state?.codexHome || "正在读取 Codex 目录..."}</p>
          </div>
        </div>
        <div className="top-actions">
          <StatusPill tone={state?.authExists ? "ok" : "neutral"}>
            auth.json {state?.authExists ? "存在" : "未存在"}
          </StatusPill>
          <StatusPill tone={runtimeTone}>
            同步 {state?.runtime?.syncEngine || state?.runtime?.syncPackage || "-"}
          </StatusPill>
          <button className="icon-button" onClick={refresh} disabled={isBusy} title="刷新">
            <RefreshCcw size={18} />
          </button>
        </div>
      </header>

      <div className="workspace-grid">
        <section className="main-flow">
          <section className="tool-section">
            <div className="section-title">
              <DatabaseZap size={19} />
              <span>压缩问题修复</span>
              {openAIProvider ? <StatusPill tone="warn">检测到 OpenAI</StatusPill> : <StatusPill tone="ok">Provider 已避开 OpenAI</StatusPill>}
            </div>
            <div className="control-strip">
              <label>
                <span>新 Provider 名称</span>
                <input value={customName} onChange={(event) => setCustomName(event.target.value)} placeholder="simplaj" />
              </label>
              <button
                className="primary-button"
                disabled={!canUseConfig || isBusy}
                onClick={() => runAction("重命名 Provider 并同步聊天记录", async () => {
                  const result = await api.repairProvider({ customName, runSync: true });
                  return {
                    message: result.message,
                    providerId: result.providerId,
                    backupDir: result.backupDir,
                    sync: formatOutput(result.sync)
                  };
                })}
              >
                {busy === "重命名 Provider 并同步聊天记录" ? <Loader2 className="spin" size={17} /> : <Wrench size={17} />}
                一键修复并同步
              </button>
              <button
                className="secondary-button"
                disabled={!canUseConfig || isBusy}
                onClick={() => runAction("仅运行原生同步", async () => {
                  const result = await api.runProviderSync({ command: "sync", providerId: selectedProviderValue });
                  return formatOutput(result);
                })}
              >
                <RefreshCcw size={17} />
                仅同步
              </button>
            </div>
            <ProviderTable providers={providers} rootProvider={rootProvider} />
          </section>

          <section className="tool-section">
            <div className="section-title">
              <ShieldCheck size={19} />
              <span>远程控制与插件解锁</span>
              <StatusPill tone="neutral">auth 轮换</StatusPill>
            </div>
            <div className="step-grid">
              <div className="step-block">
                <div className="step-head">
                  <Archive size={18} />
                  <strong>1. 备份并移除 auth.json</strong>
                </div>
                <button
                  className="secondary-button full"
                  disabled={isBusy}
                  onClick={() => runAction("备份并移除 auth.json", () => api.backupAndRemoveAuth())}
                >
                  <Archive size={17} />
                  执行备份移除
                </button>
                <p className="step-note">完成后重启 Codex App，并登录 GPT 账号；需要远程控制时使用和手机一致的账号。</p>
              </div>

              <div className="step-block">
                <div className="step-head">
                  <KeyRound size={18} />
                  <strong>2. 写入 experimental_bearer_token</strong>
                </div>
                <div className="stacked-controls">
                  <label>
                    <span>目标 Provider</span>
                    <select value={selectedProviderValue} onChange={(event) => setSelectedProvider(event.target.value)}>
                      {providers.map((provider) => (
                        <option key={provider.id} value={provider.id}>{provider.id}</option>
                      ))}
                      {!providers.length ? <option value="simplaj">simplaj</option> : null}
                    </select>
                  </label>
                  <div className="candidate-row">
                    <button className="ghost-button" disabled={isBusy} onClick={loadTokenCandidates}>
                      <RefreshCcw size={16} />
                      读取备份 key
                    </button>
                    <select value={selectedCandidate} onChange={(event) => setSelectedCandidate(event.target.value)}>
                      <option value="">手动输入</option>
                      {tokenCandidates.map((candidate) => (
                        <option key={candidate.id} value={candidate.id}>
                          {candidate.masked} · {candidate.backupDir.split(/[\\/]/).at(-1)}
                        </option>
                      ))}
                    </select>
                  </div>
                  {!selectedCandidate ? (
                    <input
                      type="password"
                      value={manualToken}
                      onChange={(event) => setManualToken(event.target.value)}
                      placeholder="sk-..."
                    />
                  ) : null}
                  <button
                    className="primary-button full"
                    disabled={isBusy || (!selectedCandidate && !manualToken)}
                    onClick={() => runAction("写入 experimental_bearer_token", () => api.applyExperimentalToken({
                      providerId: selectedProviderValue,
                      candidateId: selectedCandidate || undefined,
                      token: selectedCandidate ? undefined : manualToken
                    }))}
                  >
                    <KeyRound size={17} />
                    写入 token
                  </button>
                </div>
                <p className="step-note">写入后重启 Codex App，让远程控制和插件能力读取新的 provider 认证。</p>
              </div>
            </div>
          </section>
        </section>

        <aside className="side-panel">
          <section className="status-panel">
            <div className="section-title">
              <CheckCircle2 size={18} />
              <span>当前状态</span>
            </div>
            <div className="path-list">
              <button onClick={() => api.openPath({ targetPath: state?.configPath })} disabled={!state?.configExists}>
                <FolderOpen size={16} />
                <span>config.toml</span>
              </button>
              <button onClick={() => api.openPath({ targetPath: state?.backupRoot })} disabled={!state?.backupDirs?.length}>
                <FolderOpen size={16} />
                <span>工具备份目录</span>
              </button>
            </div>
            <dl className="facts">
              <div><dt>当前 Provider</dt><dd>{rootProvider || "-"}</dd></div>
              <div><dt>同步引擎</dt><dd>{state?.runtime?.syncEngine || state?.runtime?.syncPackage || "native-rust-rusqlite"}</dd></div>
              <div><dt>Node/npx</dt><dd>打包应用不需要</dd></div>
              <div><dt>Tauri 后端</dt><dd>Rust</dd></div>
            </dl>
            <button
              className="secondary-button full"
              disabled={isBusy}
              onClick={() => runAction("原生同步 status", async () => {
                const result = await api.runProviderSync({ command: "status" });
                return formatOutput(result);
              })}
            >
              <TerminalSquare size={17} />
              查看同步状态
            </button>
          </section>

          <section className="status-panel">
            <div className="section-title">
              <RotateCcw size={18} />
              <span>最近备份</span>
            </div>
            <div className="backup-list">
              {state?.backupDirs?.length ? state.backupDirs.map((dir) => (
                <button key={dir} onClick={() => api.openPath({ targetPath: dir })}>
                  <span>{dir.split(/[\\/]/).at(-1)}</span>
                </button>
              )) : <div className="empty-line">暂无工具备份。</div>}
            </div>
          </section>

          <LogPanel logs={logs} onClear={() => setLogs([])} />
        </aside>
      </div>
    </main>
  );
}
