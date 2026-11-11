import { open, save as saveFile } from "@tauri-apps/plugin-dialog";
import { useCallback, useEffect, useState } from "react";

import { AppError, fromUnknown } from "../lib/errors";
import { call } from "../lib/ipc";
import type { AuthType, ConnectionDto, ConnectionInput, TestOutcome } from "../lib/types";
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

  /**
   * 从文件导入连接。已有的（主机+端口+用户名相同）跳过，不覆盖。
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

      const summary = await call<{ imported: number; skipped: number }>("config_import", {
        path: file,
      });

      await reload(query);
      bumpConnections();

      const skipped = summary.skipped > 0 ? `，跳过 ${summary.skipped} 条已存在的` : "";
      setNotice({
        tone: summary.imported > 0 ? "ok" : "warn",
        title: summary.imported > 0 ? `已导入 ${summary.imported} 条${skipped}` : `没有新增连接${skipped}`,
        detail: "导入的连接第一次使用前需要确认主机指纹；密码需要自己补填。",
      });
    } catch (err) {
      setNotice({ tone: "danger", title: fromUnknown(err).message, detail: detailOf(err) });
    } finally {
      setIoBusy(false);
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
            {items.map((item) => (
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
                  </span>
                </button>
              </li>
            ))}
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
    </div>
  );
}

function detailOf(err: unknown): string | null {
  return err instanceof AppError ? err.detail : null;
}
