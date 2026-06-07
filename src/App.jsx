import { useEffect, useMemo, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import {
  AlertTriangle,
  Archive,
  CheckCircle2,
  DatabaseZap,
  FolderOpen,
  KeyRound,
  Loader2,
  Power,
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
  codexRunning: false,
  codexProcesses: [],
  backupRoot: "",
  backupDirs: [],
  providerChoices: ["simplaj", "openai"],
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
  if (command === "try_quit_codex") {
    return {
      attempted: false,
      commands: [],
      stillRunning: false,
      processes: [],
      message: "Preview mode: 未检测到 Codex 进程。"
    };
  }
  return { opened: false };
}

const api = {
  inspect: () => callTauri("inspect"),
  repairProvider: ({ customName, runSync }) => callTauri("repair_provider", { customName, runSync }),
  runProviderSync: ({ command, providerId } = {}) => callTauri("run_provider_sync", { command, providerId }),
  backupAndRemoveAuth: () => callTauri("backup_remove_auth"),
  listAuthTokenCandidates: () => callTauri("list_auth_token_candidates"),
  tryQuitCodex: () => callTauri("try_quit_codex"),
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
  const [codexClosedForWrite, setCodexClosedForWrite] = useState(false);

  const providers = state?.config?.providers || [];
  const rootProvider = state?.config?.rootProvider || "";
  const providerChoices = useMemo(() => {
    const choices = new Set([
      ...(state?.providerChoices || []),
      ...providers.map((provider) => provider.id),
      rootProvider,
      "simplaj"
    ].filter(Boolean));
    return [...choices].sort((left, right) => {
      if (left === rootProvider) return -1;
      if (right === rootProvider) return 1;
      return left.localeCompare(right);
    });
  }, [state?.providerChoices, providers, rootProvider]);
  const openAIProvider = useMemo(() => (
    providers.find((provider) => (
      provider.id.toLowerCase() === "openai" || String(provider.name).toLowerCase() === "openai"
    ))
  ), [providers]);
  const selectedProviderValue = selectedProvider || rootProvider || providerChoices[0] || providers[0]?.id || "simplaj";
  const currentSyncProvider = rootProvider || selectedProviderValue || "simplaj";

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

  async function requestCodexQuit() {
    return runAction("尝试退出 Codex", async () => {
      const result = await api.tryQuitCodex();
      setCodexClosedForWrite(!result.stillRunning);
      return result;
    });
  }

  const runtimeTone = state?.runtime?.nodeRequiredForSync ? "warn" : "ok";
  const canUseConfig = Boolean(state?.configExists);
  const isBusy = Boolean(busy);
  const codexRunning = Boolean(state?.codexRunning);
  const canWriteCodexState = codexClosedForWrite && !codexRunning && canUseConfig;

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
          <section className="tool-section attention-section">
            <div className="section-title">
              <AlertTriangle size={19} />
              <span>写入前确认</span>
              <StatusPill tone={canWriteCodexState ? "ok" : "warn"}>
                {canWriteCodexState ? "可写入" : "需关闭 Codex"}
              </StatusPill>
            </div>
            <div className="close-actions">
              <button className="secondary-button" disabled={isBusy} onClick={requestCodexQuit}>
                {busy === "尝试退出 Codex" ? <Loader2 className="spin" size={17} /> : <Power size={17} />}
                尝试退出 Codex
              </button>
              <StatusPill tone={codexRunning ? "warn" : "ok"}>
                {codexRunning ? `检测到 ${state?.codexProcesses?.length || 0} 个进程` : "未检测到进程"}
              </StatusPill>
            </div>
            <label className="close-confirm">
              <input
                type="checkbox"
                checked={codexClosedForWrite}
                onChange={(event) => setCodexClosedForWrite(event.target.checked)}
                disabled={codexRunning}
              />
              <span>已完全退出 Codex App、Codex CLI 和 app-server，可以写入配置与历史索引。</span>
            </label>
            {codexRunning ? (
              <div className="process-list">
                {(state?.codexProcesses || []).slice(0, 6).map((process) => (
                  <div key={`${process.pid}:${process.command}`}>
                    <b>{process.pid}</b>
                    <span>{process.command}</span>
                  </div>
                ))}
              </div>
            ) : null}
            <p className="step-note">
              顺序：先退出 Codex，再重命名 provider 并同步历史；远程/插件解锁时先移除 auth.json，打开 Codex 登录 GPT，登录完成后再次退出 Codex，再回来写入 key。
            </p>
          </section>

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
              <label>
                <span>当前 Provider</span>
                <input value={currentSyncProvider} readOnly />
              </label>
              <button
                className="primary-button"
                disabled={!canWriteCodexState || isBusy}
                onClick={() => runAction("重命名 Provider 并同步聊天记录", async () => {
                  const result = await api.repairProvider({ customName, runSync: true });
                  setSelectedProvider(result.providerId);
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
                disabled={!canWriteCodexState || isBusy}
                onClick={() => runAction("仅运行原生同步", async () => {
                  const result = await api.runProviderSync({ command: "sync", providerId: currentSyncProvider });
                  return formatOutput(result);
                })}
              >
                <RefreshCcw size={17} />
                仅同步
              </button>
            </div>
            <ProviderTable providers={providers} rootProvider={rootProvider} />
            {!codexClosedForWrite ? (
              <div className="warning-box">
                <AlertTriangle size={16} />
                <span>同步会写入 state_5.sqlite、会话 rollout 和项目缓存。请先完全关闭 Codex。</span>
              </div>
            ) : null}
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
                  disabled={isBusy || !codexClosedForWrite}
                  onClick={() => runAction("备份并移除 auth.json", () => api.backupAndRemoveAuth())}
                >
                  <Archive size={17} />
                  执行备份移除
                </button>
                <p className="step-note">执行前关闭 Codex。完成后打开 Codex 登录 GPT 账号；需要远程控制时使用和手机一致的账号。登录完成后再次关闭 Codex。</p>
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
                      {providerChoices.map((providerId) => (
                        <option key={providerId} value={providerId}>{providerId}</option>
                      ))}
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
                    disabled={isBusy || !codexClosedForWrite || (!selectedCandidate && !manualToken)}
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
                <p className="step-note">只在 GPT 登录完成且 Codex 已关闭时写入。写入后再启动 Codex App，让远程控制和插件能力读取新的 provider 认证。</p>
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
              <div><dt>可同步 Provider</dt><dd>{providerChoices.length}</dd></div>
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
