import { listen } from "@tauri-apps/api/event";
import { open, save as saveFile } from "@tauri-apps/plugin-dialog";
import { useCallback, useEffect, useState } from "react";

import { AppError, fromUnknown } from "../lib/errors";
import { call } from "../lib/ipc";
import { jumpCandidates, jumpName } from "../lib/jump";
import type {
  AuthType,
  ConnectionDto,
  ConnectionInput,
  ImportCandidate,
  ImportPreview,
  TestOutcome,
  TunnelDto,
} from "../lib/types";
import { useUiStore } from "../stores/uiStore";

interface FormState {
  name: string;
  host: string;
  port: string;
  username: string;
  authType: AuthType;
  password: string;
  keyPath: string;
  keyPassphrase: string;
  legacyAlgorithms: boolean;
  /** 空串 = 直连。下拉框的值只能是字符串，转成 `null` 的活留给 toInput */
  jumpConnectionId: string;
}

type Tone = "ok" | "warn" | "danger";

interface Notice {
  tone: Tone;
  title: string;
  detail?: string | null;
  /** 只有指纹变更时才给「清除记录」这个出口 */
  action?: "forget";
}

const EMPTY_FORM: FormState = {
  name: "",
  host: "",
  port: "22",
  username: "",
  authType: "password",
  password: "",
  keyPath: "",
  keyPassphrase: "",
  legacyAlgorithms: false,
  jumpConnectionId: "",
};

function formFrom(dto: ConnectionDto): FormState {
  return {
    name: dto.name,
    host: dto.host,
    port: String(dto.port),
    username: dto.username,
    authType: dto.authType,
    password: "",
    keyPath: dto.keyPath ?? "",
    keyPassphrase: "",
    legacyAlgorithms: dto.legacyAlgorithms,
    jumpConnectionId: dto.jumpConnectionId ?? "",
  };
}

/**
 * 敏感字段只在用户真的打了字时才发送：
 * 留空 = 保持库里原值（后端会回落到已保存的密文），新建时必须填。
 */
function toInput(form: FormState, editing: boolean): ConnectionInput {
  const input: ConnectionInput = {
    name: form.name.trim(),
    host: form.host.trim(),
    port: Number.parseInt(form.port, 10) || 0,
    username: form.username.trim(),
    authType: form.authType,
    legacyAlgorithms: form.legacyAlgorithms,
    jumpConnectionId: form.jumpConnectionId || null,
  };

  if (form.authType === "password") {
    if (form.password !== "" || !editing) {
      input.password = form.password;
    }
  } else {
    input.keyPath = form.keyPath.trim();
    if (form.keyPassphrase !== "" || !editing) {
      input.keyPassphrase = form.keyPassphrase;
    }
  }

  return input;
}

export default function ConnectionsPage() {
  const [items, setItems] = useState<ConnectionDto[]>([]);
  const [query, setQuery] = useState("");
  const [loading, setLoading] = useState(true);

  const [editingId, setEditingId] = useState<string | null>(null);
  const [form, setForm] = useState<FormState | null>(null);

  const [busy, setBusy] = useState(false);
  const [notice, setNotice] = useState<Notice | null>(null);
  /** 导入/导出进行中，按钮置灰，避免连点产生两份文件 */
  const [ioBusy, setIoBusy] = useState(false);
  const [pendingTrust, setPendingTrust] = useState<{ algo: string; fingerprint: string } | null>(
    null,
  );
  const [confirmDelete, setConfirmDelete] = useState(false);
  /**
   * 导入前待确认的跳板配对。
   *
   * 导出文件里只有跳板的**名字**（id 跨机器没意义），所以导入时不能自动还原：
   * 同名不代表同一台机器，猜错会静默连到别处。这里把线索摆出来让用户点一下。
   */
  /** 当前连接的本地转发规则（带运行状态） */
  const [tunnels, setTunnels] = useState<TunnelDto[]>([]);
  const [tunnelDraft, setTunnelDraft] = useState({
    localPort: "",
    targetHost: "",
    targetPort: "",
  });
  const [tunnelBusy, setTunnelBusy] = useState(false);
  const [pendingImport, setPendingImport] = useState<{
    path: string;
    candidates: ImportCandidate[];
    rows: { key: string; name: string; jumpName: string; choice: string }[];
  } | null>(null);
  // 终端页常驻挂载，落库后必须发信号让它重新拉列表，否则新连接在终端页看不见
  const bumpConnections = useUiStore((state) => state.bumpConnections);

  const reload = useCallback(async (search: string) => {
    setLoading(true);
    try {
      const rows = await call<ConnectionDto[]>("connection_list", { query: search || null });
      setItems(rows);
    } catch (err) {
      setNotice({ tone: "danger", title: fromUnknown(err).message });
    } finally {
      setLoading(false);
    }
  }, []);

  useEffect(() => {
    void reload(query);
  }, [query, reload]);

  /**
   * 导出全部连接。
   *
   * 文件里**没有密码、没有主机指纹** —— 只有"怎么连到哪台机器"。
   * 换电脑后第一次连接要重新输密码、重新确认指纹，这是刻意的（见 config_io 模块头）。
   */
  const exportConnections = async () => {
    setIoBusy(true);
    setNotice(null);
    try {
      const picked = await saveFile({
        title: "导出连接配置",
        defaultPath: "termharbor-connections.json",
        filters: [{ name: "JSON", extensions: ["json"] }],
      });
      if (!picked) return;

      const summary = await call<{ connections: number; path: string }>("config_export", {
        path: picked,
      });
      setNotice({
        tone: "ok",
        title: `已导出 ${summary.connections} 条连接`,
        detail: "文件里不含密码与主机指纹；换电脑导入后首次连接需要重新输密码、确认指纹。",
      });
    } catch (err) {
      setNotice({ tone: "danger", title: fromUnknown(err).message, detail: detailOf(err) });
    } finally {
      setIoBusy(false);
    }
  };

  /** 真正落库：`jumps` 是用户确认过的配对（key → 现有连接 id） */
  const runImport = async (path: string, jumps: Record<string, string>) => {
    const summary = await call<{ imported: number; skipped: number }>("config_import", {
      path,
      jumps: Object.keys(jumps).length > 0 ? jumps : null,
    });

    await reload(query);
    bumpConnections();

    const skipped = summary.skipped > 0 ? `，跳过 ${summary.skipped} 条已存在的` : "";
    const linked = Object.keys(jumps).length;
    const paired = linked > 0 ? `，其中 ${linked} 条已配好跳板` : "";
    setNotice({
      tone: summary.imported > 0 ? "ok" : "warn",
      title:
        summary.imported > 0
          ? `已导入 ${summary.imported} 条${paired}${skipped}`
          : `没有新增连接${skipped}`,
      detail: "导入的连接第一次使用前需要确认主机指纹；密码需要自己补填。",
    });
  };

  /**
   * 从文件导入连接。已有的（主机+端口+用户名相同）跳过，不覆盖。
   *
   * 文件里带跳板名字时先走预览：让用户确认「原本经谁」对应本机的哪条连接，
   * 而不是把依赖悄悄丢掉。名字对不上就按直连导入。
   */
  const importConnections = async () => {
    setIoBusy(true);
    setNotice(null);
    try {
      const picked = await open({
        title: "导入连接配置",
        multiple: false,
        directory: false,
        filters: [{ name: "JSON", extensions: ["json"] }],
      });
      const file = Array.isArray(picked) ? picked[0] : picked;
      if (!file) return;

      const preview = await call<ImportPreview>("config_import_preview", { path: file });
      // 会被跳过的不问：它们根本不会进库
      const rows = preview.entries
        .filter((entry) => entry.jumpName && !entry.duplicate)
        .map((entry) => ({
          key: entry.key,
          name: entry.name,
          jumpName: entry.jumpName ?? "",
          choice: entry.suggested ?? "",
        }));

      if (rows.length === 0) {
        await runImport(file, {});
        return;
      }

      setPendingImport({ path: file, candidates: preview.connections, rows });
    } catch (err) {
      setNotice({ tone: "danger", title: fromUnknown(err).message, detail: detailOf(err) });
    } finally {
      setIoBusy(false);
    }
  };

  const confirmImport = async () => {
    const pending = pendingImport;
    if (!pending) return;

    const jumps: Record<string, string> = {};
    for (const row of pending.rows) {
      if (row.choice) jumps[row.key] = row.choice;
    }

    setIoBusy(true);
    try {
      await runImport(pending.path, jumps);
      setPendingImport(null);
    } catch (err) {
      setNotice({ tone: "danger", title: fromUnknown(err).message, detail: detailOf(err) });
    } finally {
      setIoBusy(false);
    }
  };

  const patchImportRow = (key: string, choice: string) => {
    setPendingImport((current) =>
      current
        ? {
            ...current,
            rows: current.rows.map((row) => (row.key === key ? { ...row, choice } : row)),
          }
        : current,
    );
  };

  const reloadTunnels = useCallback(async (connectionId: string | null) => {
    if (!connectionId) {
      setTunnels([]);
      return;
    }
    try {
      setTunnels(await call<TunnelDto[]>("tunnel_list", { connectionId }));
    } catch {
      // 列表拉不到不该打断编辑，静默留空即可
      setTunnels([]);
    }
  }, []);

  useEffect(() => {
    void reloadTunnels(editingId);
  }, [editingId, reloadTunnels]);

  // 隧道状态可能在后台自己变（断线重连、重连成功），不经过任何一次点击，
  // 所以得听事件，否则界面会一直停在最后一次操作的结果上。
  useEffect(() => {
    let disposed = false;
    let unlisten: (() => void) | undefined;
    void listen<{ tunnelId: string; state: TunnelDto["state"]; message: string | null }>(
      "tunnel:state",
      (event) => {
        const { tunnelId, state, message } = event.payload;
        setTunnels((prev) =>
          prev.map((item) => (item.id === tunnelId ? { ...item, state, message } : item)),
        );
      },
    ).then((fn) => {
      if (disposed) fn();
      else unlisten = fn;
    });
    return () => {
      disposed = true;
      unlisten?.();
    };
  }, []);

  const addTunnel = async () => {
    if (!editingId) return;
    const localPort = Number.parseInt(tunnelDraft.localPort, 10);
    const targetPort = Number.parseInt(tunnelDraft.targetPort, 10);
    if (!localPort || !targetPort || !tunnelDraft.targetHost.trim()) {
      setNotice({ tone: "warn", title: "本机端口、目标地址、目标端口都要填" });
      return;
    }

    setTunnelBusy(true);
    try {
      await call<TunnelDto>("tunnel_create", {
        connectionId: editingId,
        localPort,
        targetHost: tunnelDraft.targetHost.trim(),
        targetPort,
      });
      setTunnelDraft({ localPort: "", targetHost: "", targetPort: "" });
      await reloadTunnels(editingId);
      setNotice(null);
    } catch (err) {
      setNotice({ tone: "danger", title: fromUnknown(err).message, detail: detailOf(err) });
    } finally {
      setTunnelBusy(false);
    }
  };

  const toggleTunnel = async (tunnel: TunnelDto) => {
    setTunnelBusy(true);
    try {
      // 重连中说明监听还在，这个按钮该是"停止"
      const running = tunnel.state === "listening" || tunnel.state === "reconnecting";
      await call<TunnelDto>(running ? "tunnel_stop" : "tunnel_start", { id: tunnel.id });
      await reloadTunnels(editingId);
      setNotice(null);
    } catch (err) {
      // 失败原因（端口被占、远端连不上）比"失败"两个字有用得多
      setNotice({ tone: "danger", title: fromUnknown(err).message, detail: detailOf(err) });
      await reloadTunnels(editingId);
    } finally {
      setTunnelBusy(false);
    }
  };

  /// 自动启动只是改下次启动的行为，不立刻拉起或停掉这条规则
  const toggleAutoStart = async (tunnel: TunnelDto) => {
    setTunnelBusy(true);
    try {
      const next = await call<TunnelDto>("tunnel_set_auto_start", {
        id: tunnel.id,
        autoStart: !tunnel.autoStart,
      });
      setTunnels((prev) => prev.map((item) => (item.id === next.id ? next : item)));
      setNotice(null);
    } catch (err) {
      setNotice({ tone: "danger", title: fromUnknown(err).message, detail: detailOf(err) });
    } finally {
      setTunnelBusy(false);
    }
  };

  const removeTunnel = async (id: string) => {
    setTunnelBusy(true);
    try {
      await call("tunnel_delete", { id });
      await reloadTunnels(editingId);
    } catch (err) {
      setNotice({ tone: "danger", title: fromUnknown(err).message, detail: detailOf(err) });
    } finally {
      setTunnelBusy(false);
    }
  };

  const openNew = () => {
    setEditingId(null);
    setForm({ ...EMPTY_FORM });
    setNotice(null);
    setPendingTrust(null);
    setConfirmDelete(false);
  };

  const openEdit = (dto: ConnectionDto) => {
    setEditingId(dto.id);
    setForm(formFrom(dto));
    setNotice(null);
    setPendingTrust(null);
    setConfirmDelete(false);
  };

  const patch = (partial: Partial<FormState>) => {
    setForm((prev) => (prev ? { ...prev, ...partial } : prev));
  };

  const save = async () => {
    if (!form) return;
    setBusy(true);
    setNotice(null);
    try {
      const input = toInput(form, editingId !== null);
      const saved = editingId
        ? await call<ConnectionDto>("connection_update", { id: editingId, input })
        : await call<ConnectionDto>("connection_create", { input });

      await reload(query);
      setEditingId(saved.id);
      setForm(formFrom(saved));
      setNotice({ tone: "ok", title: "已保存，可到「终端」页打开会话" });
      bumpConnections();
    } catch (err) {
      setNotice({ tone: "danger", title: fromUnknown(err).message, detail: detailOf(err) });
    } finally {
      setBusy(false);
    }
  };

  const runTest = async (trust?: string) => {
    if (!form) return;
    setBusy(true);
    setNotice(null);
    try {
      const outcome = await call<TestOutcome>("connection_test", {
        id: editingId,
        input: toInput(form, editingId !== null),
        trustFingerprint: trust ?? null,
      });
      applyOutcome(outcome);
    } catch (err) {
      setNotice({ tone: "danger", title: fromUnknown(err).message, detail: detailOf(err) });
    } finally {
      setBusy(false);
    }
  };

  const applyOutcome = (outcome: TestOutcome) => {
    switch (outcome.status) {
      case "connected":
        setPendingTrust(null);
        setNotice({
          tone: "ok",
          title: `连接成功 · ${outcome.elapsedMs}ms`,
          detail: `${outcome.hostKeyAlgo} · ${outcome.fingerprint}${
            outcome.newlyTrusted ? "（已记录到本机）" : ""
          }`,
        });
        break;
      case "confirmation_required":
        setPendingTrust({ algo: outcome.hostKeyAlgo, fingerprint: outcome.fingerprint });
        setNotice({
          tone: "warn",
          title: "首次连接这台主机，请核对指纹",
          detail: "如果和服务器上 `ssh-keygen -lf /etc/ssh/ssh_host_*_key.pub` 输出一致，再继续。",
        });
        break;
      case "host_key_changed":
        setPendingTrust(null);
        setNotice({
          tone: "danger",
          title: "主机密钥与上次记录不一致，已阻止连接",
          detail: `本次 ${outcome.fingerprint}\n记录 ${outcome.storedFingerprint}`,
          action: "forget",
        });
        break;
      case "certificate_unsupported":
        setNotice({
          tone: "warn",
          title: "该服务器使用主机证书，当前版本暂不支持",
        });
        break;
      case "failed":
        setNotice({ tone: "danger", title: outcome.message, detail: outcome.detail });
        break;
    }
  };

  const forgetAndRetry = async () => {
    if (!form) return;
    setBusy(true);
    try {
      await call<void>("known_host_forget", { host: form.host.trim(), port: Number(form.port) });
      await runTest();
    } catch (err) {
      setNotice({ tone: "danger", title: fromUnknown(err).message });
      setBusy(false);
    }
  };

  const remove = async () => {
    if (!editingId) return;
    setBusy(true);
    try {
      await call<void>("connection_delete", { id: editingId });
      setEditingId(null);
      setForm(null);
      setConfirmDelete(false);
      await reload(query);
      setNotice({ tone: "ok", title: "已删除" });
      bumpConnections();
    } catch (err) {
      setNotice({ tone: "danger", title: fromUnknown(err).message });
    } finally {
      setBusy(false);
    }
  };

  const pickKeyFile = async () => {
    try {
      const picked = await open({
        multiple: false,
        directory: false,
        title: "选择 SSH 私钥文件",
      });
      if (typeof picked === "string") {
        patch({ keyPath: picked });
      }
    } catch (err) {
      setNotice({ tone: "danger", title: fromUnknown(err).message });
    }
  };

  const editing = editingId !== null;

  return (
    <div className="page page--split">
      <aside className="conn-list">
        <div className="conn-list__head">
          <input
            className="input input--search"
            placeholder="搜索名称 / 主机 / 用户"
            value={query}
            onChange={(event) => setQuery(event.target.value)}
          />
          <button type="button" className="btn btn--primary" onClick={openNew}>
            <svg viewBox="0 0 16 16" aria-hidden="true">
              <path d="M8 3.4v9.2M3.4 8h9.2" />
            </svg>
            新建
          </button>
        </div>

        {/* 导入导出降级成 ghost 小按钮：主按钮位只留给「新建」 */}
        <div className="conn-list__io">
          <button
            type="button"
            className="btn btn--ghost btn--sm"
            disabled={ioBusy}
            title="导出全部连接（不含密码）"
            onClick={() => void exportConnections()}
          >
            <svg viewBox="0 0 16 16" aria-hidden="true">
              <path d="M8 10.2V2.8" />
              <path d="m5.4 5.4 2.6-2.6 2.6 2.6" />
              <path d="M3.2 10.4v1.6a1.4 1.4 0 0 0 1.4 1.4h6.8a1.4 1.4 0 0 0 1.4-1.4v-1.6" />
            </svg>
            导出
          </button>
          <button
            type="button"
            className="btn btn--ghost btn--sm"
            disabled={ioBusy}
            title="从文件导入连接（已存在的跳过）"
            onClick={() => void importConnections()}
          >
            <svg viewBox="0 0 16 16" aria-hidden="true">
              <path d="M8 2.8v7.4" />
              <path d="m5.4 7.6 2.6 2.6 2.6-2.6" />
              <path d="M3.2 10.4v1.6a1.4 1.4 0 0 0 1.4 1.4h6.8a1.4 1.4 0 0 0 1.4-1.4v-1.6" />
            </svg>
            导入
          </button>
        </div>

        {loading ? (
          <p className="conn-list__hint">加载中…</p>
        ) : items.length === 0 ? (
          <p className="conn-list__hint">
            {query ? "没有匹配的连接" : "还没有连接，点「新建」添加第一台服务器"}
          </p>
        ) : (
          <ul className="conn-list__items">
            {items.map((item) => {
              const jump = jumpName(items, item.jumpConnectionId);
              return (
              <li key={item.id}>
                <button
                  type="button"
                  className={`conn-item ${item.id === editingId ? "is-active" : ""}`}
                  onClick={() => openEdit(item)}
                >
                  <span className="conn-item__name">{item.name}</span>
                  <span className="conn-item__addr">
                    {item.username}@{item.host}:{item.port}
                  </span>
                  <span className="conn-item__tags">
                    <span className="pill">{item.authType === "password" ? "密码" : "密钥"}</span>
                    {item.legacyAlgorithms ? <span className="pill pill--warn">兼容</span> : null}
                    {jump ? <span className="pill">经 {jump}</span> : null}
                  </span>
                </button>
              </li>
              );
            })}
          </ul>
        )}
      </aside>

      <section className="conn-editor">
        {!form ? (
          <div className="conn-editor__empty">
            <h2>选择或新建一条连接</h2>
            <p>密码与私钥口令会在落库前用 AES-256-GCM 加密，前端永远拿不到明文。</p>
          </div>
        ) : (
          <>
            <header className="conn-editor__head">
              <h2>{editing ? "编辑连接" : "新建连接"}</h2>
              {editing ? (
                confirmDelete ? (
                  <span className="conn-editor__confirm">
                    <button type="button" className="btn btn--danger" onClick={remove} disabled={busy}>
                      确认删除
                    </button>
                    <button
                      type="button"
                      className="btn"
                      onClick={() => setConfirmDelete(false)}
                      disabled={busy}
                    >
                      取消
                    </button>
                  </span>
                ) : (
                  <button
                    type="button"
                    className="btn btn--danger"
                    onClick={() => setConfirmDelete(true)}
                    disabled={busy}
                  >
                    删除
                  </button>
                )
              ) : null}
            </header>

            {notice ? (
              <div className={`notice notice--${notice.tone}`}>
                <strong>{notice.title}</strong>
                {notice.detail ? <pre className="notice__detail">{notice.detail}</pre> : null}
                {pendingTrust ? (
                  <div className="notice__actions">
                    <button
                      type="button"
                      className="btn btn--primary"
                      onClick={() => void runTest(pendingTrust.fingerprint)}
                      disabled={busy}
                    >
                      指纹一致，信任并连接
                    </button>
                    <button
                      type="button"
                      className="btn"
                      onClick={() => setPendingTrust(null)}
                      disabled={busy}
                    >
                      取消
                    </button>
                  </div>
                ) : null}
                {notice.action === "forget" ? (
                  <div className="notice__actions">
                    <button type="button" className="btn" onClick={forgetAndRetry} disabled={busy}>
                      清除本地记录并重新连接
                    </button>
                  </div>
                ) : null}
              </div>
            ) : null}

            <div className="field-grid">
              <label className="field">
                <span>名称</span>
                <input
                  className="input"
                  value={form.name}
                  onChange={(e) => patch({ name: e.target.value })}
                  placeholder="生产 Web-01"
                />
              </label>
              <label className="field">
                <span>主机</span>
                <input
                  className="input"
                  value={form.host}
                  onChange={(e) => patch({ host: e.target.value })}
                  placeholder="10.0.0.12 或 example.com"
                />
              </label>
              <label className="field field--narrow">
                <span>端口</span>
                <input
                  className="input"
                  value={form.port}
                  onChange={(e) => patch({ port: e.target.value.replace(/\D/g, "") })}
                  inputMode="numeric"
                />
              </label>
              <label className="field">
                <span>用户名</span>
                <input
                  className="input"
                  value={form.username}
                  onChange={(e) => patch({ username: e.target.value })}
                  placeholder="root"
                />
              </label>
            </div>

            {/* 跳板：留空就是直连。候选里排除自己与会成环的，选不到坏选项 */}
            <label className="field">
              <span>经由（跳板机）</span>
              <select
                className="input"
                value={form.jumpConnectionId}
                onChange={(e) => patch({ jumpConnectionId: e.target.value })}
              >
                <option value="">直连（不经跳板）</option>
                {jumpCandidates(items, editingId).map((item) => (
                  <option key={item.id} value={item.id}>
                    {item.name}（{item.username}@{item.host}:{item.port}）
                  </option>
                ))}
              </select>
              <em className="field__hint">
                内网机器连不上时，先选一台能连的机器当跳板。跳板本身也会连它的「经由」，
                所以可以串多跳；这台机器要先单独连通过一次，跳板的指纹才验得过。
              </em>
            </label>

            <div className="field">
              <span>认证方式</span>
              <div className="segmented">
                <button
                  type="button"
                  className={form.authType === "password" ? "is-active" : ""}
                  onClick={() => patch({ authType: "password" })}
                >
                  密码
                </button>
                <button
                  type="button"
                  className={form.authType === "key" ? "is-active" : ""}
                  onClick={() => patch({ authType: "key" })}
                >
                  私钥
                </button>
              </div>
            </div>

            {form.authType === "password" ? (
              <label className="field">
                <span>密码</span>
                <input
                  className="input"
                  type="password"
                  value={form.password}
                  onChange={(e) => patch({ password: e.target.value })}
                  placeholder={editing ? "留空则沿用已保存的密码" : ""}
                  autoComplete="new-password"
                />
              </label>
            ) : (
              <div className="field-grid">
                <label className="field">
                  <span>私钥路径</span>
                  <div className="input-with-button">
                    <input
                      className="input"
                      value={form.keyPath}
                      onChange={(e) => patch({ keyPath: e.target.value })}
                      placeholder="C:/Users/me/.ssh/id_ed25519"
                    />
                    <button type="button" className="btn" onClick={pickKeyFile}>
                      选择…
                    </button>
                  </div>
                </label>
                <label className="field">
                  <span>私钥口令（可选）</span>
                  <input
                    className="input"
                    type="password"
                    value={form.keyPassphrase}
                    onChange={(e) => patch({ keyPassphrase: e.target.value })}
                    placeholder={editing ? "留空则沿用已保存的口令" : ""}
                    autoComplete="new-password"
                  />
                </label>
              </div>
            )}

            <label className="toggle">
              <input
                type="checkbox"
                checked={form.legacyAlgorithms}
                onChange={(e) => patch({ legacyAlgorithms: e.target.checked })}
              />
              <span>
                兼容老服务器算法
                <em>只有在服务器版本很老、提示算法协商失败时才需要打开</em>
              </span>
            </label>

            {/* 本地转发：规则挂在连接上，跟这台机器走 */}
            {editing ? (
              <section className="tunnel">
                <div className="tunnel__head">
                  <span className="tunnel__title">本地转发</span>
                  <span className="field__hint">
                    把这台机器能访问的服务搬到本机。只监听 127.0.0.1，同网段的其他人连不到；
                    隧道自己建一条连接，关掉终端标签不受影响。
                  </span>
                </div>

                {tunnels.length > 0 ? (
                  <ul className="tunnel__list">
                    {tunnels.map((tunnel) => (
                      <li key={tunnel.id} className="tunnel__row">
                        <span className="tunnel__map">
                          <code>
                            127.0.0.1:{tunnel.localPort}
                          </code>
                          <span className="tunnel__arrow">→</span>
                          <code>
                            {tunnel.targetHost}:{tunnel.targetPort}
                          </code>
                        </span>
                        <label className="tunnel__auto" title="应用启动时自动拉起这条规则">
                          <input
                            type="checkbox"
                            checked={tunnel.autoStart}
                            disabled={tunnelBusy}
                            onChange={() => void toggleAutoStart(tunnel)}
                          />
                          自动
                        </label>
                        <span
                          className={`pill ${
                            tunnel.state === "listening"
                              ? "pill--ok"
                              : tunnel.state === "reconnecting"
                                ? "pill--warn"
                                : tunnel.state === "failed"
                                  ? "pill--danger"
                                  : ""
                          }`}
                        >
                          {tunnel.state === "listening"
                            ? "运行中"
                            : tunnel.state === "reconnecting"
                              ? "重连中"
                              : tunnel.state === "failed"
                                ? "失败"
                                : "未启动"}
                        </span>
                        <span className="tunnel__actions">
                          <button
                            type="button"
                            className="btn btn--small"
                            disabled={tunnelBusy}
                            onClick={() => void toggleTunnel(tunnel)}
                          >
                            {tunnel.state === "listening" || tunnel.state === "reconnecting"
                              ? "停止"
                              : "启动"}
                          </button>
                          <button
                            type="button"
                            className="btn btn--ghost btn--small"
                            disabled={tunnelBusy}
                            onClick={() => void removeTunnel(tunnel.id)}
                          >
                            删除
                          </button>
                        </span>
                        {tunnel.message && tunnel.state !== "listening" ? (
                          <p
                            className={
                              tunnel.state === "reconnecting"
                                ? "tunnel__error tunnel__error--soft"
                                : "tunnel__error"
                            }
                          >
                            {tunnel.message}
                          </p>
                        ) : null}
                      </li>
                    ))}
                  </ul>
                ) : (
                  <p className="field__hint">还没有规则。填好下面三格，点「添加」。</p>
                )}

                <div className="tunnel__add">
                  <input
                    className="input input--port"
                    placeholder="本机端口"
                    value={tunnelDraft.localPort}
                    inputMode="numeric"
                    onChange={(e) =>
                      setTunnelDraft({ ...tunnelDraft, localPort: e.target.value.replace(/\D/g, "") })
                    }
                  />
                  <input
                    className="input"
                    placeholder="目标地址，如 10.0.3.21"
                    value={tunnelDraft.targetHost}
                    onChange={(e) => setTunnelDraft({ ...tunnelDraft, targetHost: e.target.value })}
                  />
                  <input
                    className="input input--port"
                    placeholder="目标端口"
                    value={tunnelDraft.targetPort}
                    inputMode="numeric"
                    onChange={(e) =>
                      setTunnelDraft({ ...tunnelDraft, targetPort: e.target.value.replace(/\D/g, "") })
                    }
                  />
                  <button
                    type="button"
                    className="btn btn--small"
                    disabled={tunnelBusy}
                    onClick={() => void addTunnel()}
                  >
                    添加
                  </button>
                </div>
              </section>
            ) : null}

            <footer className="conn-editor__foot">
              <button
                type="button"
                className="btn btn--primary"
                onClick={save}
                disabled={busy || !form}
              >
                {editing ? "保存修改" : "保存"}
              </button>
              <button type="button" className="btn" onClick={() => void runTest()} disabled={busy}>
                测试连接
              </button>
            </footer>
          </>
        )}
      </section>

      {/* 导入配对：文件里只有跳板名字，让用户点一下对应本机哪条连接 */}
      {pendingImport ? (
        <div className="import-pair" role="dialog" aria-modal="true" aria-label="确认跳板配对">
          <div className="import-pair__card">
            <header className="import-pair__head">
              <h2>这 {pendingImport.rows.length} 条原本要经跳板</h2>
              <p>
                配置文件只记住了跳板的名字，跨机器认不出是哪台设备。请给每条选一台现有连接；
                不选就按直连导入，之后也能在连接详情里补。
              </p>
            </header>

            <ul className="import-pair__list">
              {pendingImport.rows.map((row) => (
                <li key={row.key} className="import-pair__row">
                  <div className="import-pair__target">
                    <strong>{row.name}</strong>
                    <span className="import-pair__hint">原本经「{row.jumpName}」</span>
                  </div>
                  <select
                    className="input"
                    value={row.choice}
                    onChange={(e) => patchImportRow(row.key, e.target.value)}
                  >
                    <option value="">不设（按直连导入）</option>
                    {pendingImport.candidates.map((candidate) => (
                      <option key={candidate.id} value={candidate.id}>
                        {candidate.name}（{candidate.username}@{candidate.host}:{candidate.port}）
                      </option>
                    ))}
                  </select>
                </li>
              ))}
            </ul>

            <footer className="import-pair__foot">
              <button
                type="button"
                className="btn btn--primary"
                onClick={() => void confirmImport()}
                disabled={ioBusy}
              >
                确认导入
              </button>
              <button
                type="button"
                className="btn"
                onClick={() => setPendingImport(null)}
                disabled={ioBusy}
              >
                取消
              </button>
            </footer>
          </div>
        </div>
      ) : null}
    </div>
  );
}

function detailOf(err: unknown): string | null {
  return err instanceof AppError ? err.detail : null;
}
