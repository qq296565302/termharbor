/**
 * MCP 接入设置。
 *
 * 白名单是安全边界，所以这里的每一次改动都立刻写回 `mcp.json`（后端按 mtime 热加载，
 * 不用重启 MCP 宿主）。文件本身仍是可读可备份的 JSON，高级用户可以直接改；
 * 这个页面只是让人不必手写。
 *
 * key 统一用连接 **id**：名称可以重复，用名称做安全边界会出错。加载时把老的
 * name-key 配置归一化成 id-key。
 */

import { useCallback, useEffect, useMemo, useRef, useState } from "react";

import { fromUnknown } from "../lib/errors";
import { call } from "../lib/ipc";
import { leaseLabel, REASON_LABEL } from "../lib/mcpAudit";
import type {
  McpAuditEntry,
  McpAuditView,
  McpConfig,
  McpConfigView,
  McpConnectionPolicy,
} from "../lib/types";
import McpApprovalCard from "./McpApprovalCard";

const TOOLS: { id: string; label: string; hint: string }[] = [
  { id: "list_connections", label: "列连接", hint: "查看有哪些可用服务器" },
  { id: "sftp_list", label: "列目录", hint: "读远端目录结构" },
  { id: "sftp_read", label: "读文件", hint: "读远端文本文件" },
  { id: "sftp_write", label: "写文件", hint: "覆盖写远端文本文件" },
  { id: "sftp_upload", label: "上传", hint: "本机文件传到服务器" },
  { id: "sftp_download", label: "下载", hint: "服务器文件取回本机" },
  { id: "sftp_mkdir", label: "建目录", hint: "远端新建目录" },
  { id: "sftp_rename", label: "重命名", hint: "远端改名或移动" },
  { id: "load_snapshot", label: "负载快照", hint: "CPU / 内存 / 磁盘" },
  { id: "exec", label: "执行命令", hint: "执行单条命令" },
];

function emptyPolicy(fullTrust = false): McpConnectionPolicy {
  return {
    enabled: false,
    tools: [],
    paths: [],
    allowSecretFiles: false,
    // 新配置默认开边界；v1 老配置由 normalize 传 true，升级不静默收窄。
    fullTrust,
    allowExec: true,
    allowWrite: true,
    allowUpload: true,
    exec: { allow: [], timeoutSecs: 60, maxOutputKb: 64 },
  };
}

function normalize(view: McpConfigView): McpConfig {
  // 后端 ConfigStore::load 已经把 v1 老配置的 fullTrust 补成 true 并升到 v2，
  // 前端不要再按 version 猜一次（那样会把刚迁移完的配置误判成新配置）。
  // 这里 base 只用于「配置里压根没有这台连接」的情况：新启用的一律默认开边界。
  const base = emptyPolicy();
  const connections: Record<string, McpConnectionPolicy> = {};
  for (const item of view.connections) {
    const existing = view.config.connections[item.id] ?? view.config.connections[item.name];
    connections[item.id] = existing
      ? { ...base, ...existing, exec: { ...base.exec, ...existing.exec } }
      : { ...base };
  }
  return {
    ...view.config,
    hostToken: view.config.hostToken ?? "",
    approvalTtlMinutes: view.config.approvalTtlMinutes ?? 0,
    approvals: view.config.approvals ?? [],
    connections,
  };
}

function toggle(list: string[], value: string): string[] {
  return list.includes(value) ? list.filter((item) => item !== value) : [...list, value];
}

/** 折叠状态下的一行摘要：空列表就是「不限」，必须直说。 */
function summarize(policy: McpConnectionPolicy): string {
  const parts: string[] = [];
  parts.push(policy.tools.length === 0 ? "全部工具" : `${policy.tools.length} 个工具`);
  parts.push(policy.paths.length === 0 ? "任意路径" : `${policy.paths.length} 个目录`);
  if (policy.tools.length === 0 || policy.tools.includes("exec")) {
    parts.push(
      policy.exec.allow.length === 0 ? "任意单条命令" : `${policy.exec.allow.length} 条命令`,
    );
  }
  if (policy.allowSecretFiles) parts.push("含凭据文件");
  parts.push(policy.fullTrust ? "完全信任" : "已开写入边界");
  const disabled: string[] = [];
  if (!policy.allowExec) disabled.push("执行");
  if (!policy.allowWrite) disabled.push("写");
  if (!policy.allowUpload) disabled.push("上传");
  if (disabled.length > 0) parts.push(`已关 ${disabled.join("/")}`);
  return parts.join(" · ");
}

function clampNumber(value: number, min: number, max: number): number {
  if (!Number.isFinite(value)) return min;
  return Math.min(max, Math.max(min, Math.round(value)));
}

export default function McpSettings() {
  const [config, setConfig] = useState<McpConfig | null>(null);
  const [connections, setConnections] = useState<McpConfigView["connections"]>([]);
  const [templates, setTemplates] = useState<string[]>([]);
  const [configPath, setConfigPath] = useState("");
  const [auditPath, setAuditPath] = useState("");
  /** 最近调用（放行 + 拒绝）。只列拒绝的话，伪造租约蒙混过去的那次恰恰看不到。 */
  const [calls, setCalls] = useState<McpAuditEntry[]>([]);
  const [onlyDenied, setOnlyDenied] = useState(false);
  /** 本进程 pid 与启动时刻：用来判断某条记录的租约是不是本进程写的 */
  const [selfPid, setSelfPid] = useState(0);
  const [startedAt, setStartedAt] = useState(0);
  /** 客户端此刻连着的连接 id。在线校验靠它给每台机器打点。 */
  const [live, setLive] = useState<string[]>([]);
  const [error, setError] = useState<string | null>(null);
  const [saved, setSaved] = useState(true);
  const [draftPath, setDraftPath] = useState<Record<string, string>>({});
  const [draftCommand, setDraftCommand] = useState<Record<string, string>>({});
  const [expanded, setExpanded] = useState<Record<string, boolean>>({});
  const loaded = useRef(false);
  const saveTimer = useRef<number | null>(null);

  const load = useCallback(async () => {
    loaded.current = false;
    try {
      const [view, audit, opened] = await Promise.all([
        call<McpConfigView>("mcp_config_get"),
        call<McpAuditView>("mcp_audit_recent"),
        call<string[]>("mcp_live_connections"),
      ]);
      setConfig(normalize(view));
      setConnections(view.connections);
      setTemplates(view.commandTemplates);
      setConfigPath(view.configPath);
      setAuditPath(view.auditPath);
      // 参数错是调用方自己的问题，列出来只会淹没真正值得看的记录
      setCalls(audit.entries.filter((item) => item.reason !== "invalid_params"));
      setSelfPid(audit.selfPid);
      setStartedAt(audit.startedAtMs);
      setLive(opened);
      setError(null);
      // 加载本身不算改动，别触发一次无意义的写回
      requestAnimationFrame(() => {
        loaded.current = true;
      });
    } catch (err) {
      setError(fromUnknown(err).message);
    }
  }, []);

  useEffect(() => {
    void load();
  }, [load]);

  // 在线状态每 3 秒刷一次。这个页面开着的时候，用户往往就在另一个标签页里
  // 连/断连接；不刷新的话「明明连着却显示未连接」，比不显示更让人怀疑功能坏了。
  useEffect(() => {
    const timer = window.setInterval(() => {
      call<string[]>("mcp_live_connections")
        .then(setLive)
        .catch(() => {
          // 纯只读状态，失败就保留上一次的值，不打扰用户
        });
    }, 3000);
    return () => window.clearInterval(timer);
  }, []);

  // 改动后 400ms 自动写回。不设「保存」按钮：改了就该生效。
  useEffect(() => {
    if (!config || !loaded.current) return;
    setSaved(false);
    if (saveTimer.current !== null) window.clearTimeout(saveTimer.current);
    saveTimer.current = window.setTimeout(async () => {
      try {
        await call("mcp_config_set", { config });
        setSaved(true);
        setError(null);
      } catch (err) {
        setError(fromUnknown(err).message);
      }
    }, 400);
    return () => {
      if (saveTimer.current !== null) window.clearTimeout(saveTimer.current);
    };
  }, [config]);

  const patch = (id: string, change: Partial<McpConnectionPolicy>) => {
    setConfig((current) => {
      if (!current) return current;
      const policy = current.connections[id] ?? emptyPolicy();
      return {
        ...current,
        connections: { ...current.connections, [id]: { ...policy, ...change } },
      };
    });
  };

  const patchExec = (id: string, change: Partial<McpConnectionPolicy["exec"]>) => {
    setConfig((current) => {
      if (!current) return current;
      const policy = current.connections[id] ?? emptyPolicy();
      return {
        ...current,
        connections: {
          ...current.connections,
          [id]: { ...policy, exec: { ...policy.exec, ...change } },
        },
      };
    });
  };

  const allowDenied = async (entry: McpAuditEntry) => {
    const target = connections.find((item) => item.name === entry.connection);
    if (!target || !entry.args) return;
    try {
      await call("mcp_allow_command", { connection: target.id, command: entry.args });
      await load();
    } catch (err) {
      setError(fromUnknown(err).message);
    }
  };

  const copyHostConfig = async () => {
    const exe = "%LOCALAPPDATA%\\TermHarbor\\termharbor.exe";
    // 凭证跟着宿主配置一起复制。没生成过就退回落配置（后端会放行但记警告）。
    const args = ["--mcp"];
    if (config?.hostToken) args.push("--mcp-token", config.hostToken);
    const text = JSON.stringify(
      { mcpServers: { termharbor: { command: exe, args } } },
      null,
      2,
    );
    try {
      await navigator.clipboard.writeText(text);
      setError(null);
    } catch {
      setError("复制失败，请手动填写宿主配置");
    }
  };

  /** 换凭证后旧宿主配置立刻失效，所以顺手提醒重新复制一次。 */
  const regenToken = async () => {
    try {
      const token = await call<string>("mcp_token_regenerate");
      setConfig((current) => (current ? { ...current, hostToken: token } : current));
      setError(null);
    } catch (err) {
      setError(fromUnknown(err).message);
    }
  };

  /** 一个都没开时的起步按钮：一次给全，不再让人逐项点。 */
  const quickStart = () => {
    setConfig((current) => {
      if (!current) return current;
      const next: Record<string, McpConnectionPolicy> = {};
      for (const item of connections) {
        const policy = current.connections[item.id] ?? emptyPolicy();
        next[item.id] = { ...policy, enabled: true };
      }
      return { ...current, connections: next };
    });
  };

  const enabledCount = useMemo(
    () => (config ? Object.values(config.connections).filter((p) => p.enabled).length : 0),
    [config],
  );

  // 最新在上：用户看这个列表是在问「刚才发生了什么」，不是看流水账。
  const shown = useMemo(() => {
    const list = onlyDenied ? calls.filter((item) => item.decision === "denied") : calls;
    return [...list].reverse();
  }, [calls, onlyDenied]);

  return (
    <section className="settings__group mcp">
      <div className="settings__label">
        <span>MCP 接入</span>
        <span className="settings__value">
          {enabledCount} 台已开放 · {saved ? "已保存" : "保存中…"}
        </span>
      </div>

      <p className="settings__notice">
        勾选一台服务器，就是把它整台交给智能体用：读文件、传文件、执行命令都在内。
        没勾的连接，智能体看不到也连不上。改动立即生效，无需重启宿主。
        配置放产品根目录，卸载不会清掉：
        <span className="settings__path">{configPath}</span>
      </p>

      {config ? (
        <div className="mcp__gate">
          <label className="mcp__gate-switch">
            <input
              type="checkbox"
              checked={config.requireLiveSession}
              onChange={(event) =>
                setConfig((current) =>
                  current ? { ...current, requireLiveSession: event.target.checked } : current,
                )
              }
            />
            只允许使用当前已连接的服务器
          </label>
          <span className={`mcp__gate-state${config.requireLiveSession ? "" : " is-off"}`}>
            {config.requireLiveSession ? `${live.length} 台在线` : "已关闭"}
          </span>
          <p className="mcp__gate-hint">
            {config.requireLiveSession
              ? "智能体看得到连接列表，但只有你在客户端里连着的机器才动得了。关掉连接、或者退出客户端之后立即失效，直到你下次重连。"
              : "已关闭：客户端不运行时智能体照样能连服务器。只有没人值守的自动化才需要这样。"}
          </p>
        </div>
      ) : null}

      {config ? (
        <div className="mcp__gate mcp__security">
          <div className="mcp__gate-switch">
            <span>stdio 启动凭证</span>
            <span className={`mcp__gate-state${config.hostToken ? "" : " is-off"}`}>
              {config.hostToken ? "已生成" : "未配置"}
            </span>
          </div>
          <p className="mcp__gate-hint">
            宿主拉起 MCP 时要带上这个凭证，手工启动、不带凭证的进程会被直接拒绝。
            老配置没生成过也能用，但每次启动会记一条警告。重新生成后请再点一次「复制宿主配置」。
          </p>
          <div className="mcp__quick">
            <button type="button" className="btn btn--small" onClick={() => void regenToken()}>
              {config.hostToken ? "重新生成凭证" : "生成凭证"}
            </button>
            {config.hostToken ? (
              <code className="mcp__token" title="只显示前 8 位，完整值在复制的宿主配置里">
                {config.hostToken.slice(0, 8)}…
              </code>
            ) : null}
          </div>
          <label className="mcp__ttl">
            <span>一次放行有效时长</span>
            <select
              className="mcp__ttl-select"
              value={config.approvalTtlMinutes}
              onChange={(event) =>
                setConfig((current) =>
                  current
                    ? { ...current, approvalTtlMinutes: Number(event.target.value) }
                    : current,
                )
              }
            >
              <option value={0}>本次客户端会话</option>
              <option value={10}>10 分钟</option>
              <option value={60}>1 小时</option>
              <option value={480}>8 小时</option>
            </select>
          </label>
          <p className="mcp__gate-hint">
            高危写入被拦下后，点一次「允许这一次」在这段时间内不再重复问；关掉客户端或换机器会重新问。
          </p>
        </div>
      ) : null}

      {config && enabledCount === 0 && connections.length > 0 ? (
        <p className="settings__notice mcp__quick">
          当前所有连接都未开放，智能体还看不到任何服务器。
          <button type="button" className="btn btn--small" onClick={quickStart}>
            全部开放
          </button>
          之后可在每台机器上单独收窄范围。
        </p>
      ) : null}

      {error ? <p className="settings__error">{error}</p> : null}

      {config ? (
        <div className="mcp__list">
          {connections.length === 0 ? (
            <p className="mcp__empty">还没有保存任何连接，先去「连接」页添加。</p>
          ) : (
            connections.map((item) => {
              const policy = config.connections[item.id] ?? emptyPolicy();
              return (
                <div className={`mcp__card${policy.enabled ? " is-on" : ""}`} key={item.id}>
                  <label className="mcp__head">
                    <input
                      type="checkbox"
                      checked={policy.enabled}
                      onChange={(event) => patch(item.id, { enabled: event.target.checked })}
                    />
                    <span className="mcp__name">{item.name}</span>
                    {config.requireLiveSession && policy.enabled ? (
                      <span
                        className={`mcp__live${live.includes(item.id) ? " is-on" : ""}`}
                        title={
                          live.includes(item.id)
                            ? "客户端此刻连着这台机器，智能体可以用"
                            : "客户端此刻没连这台机器，智能体调工具会被拒绝（no_live_session）"
                        }
                      >
                        <span className="mcp__live-dot" />
                        {live.includes(item.id) ? "已连接" : "未连接"}
                      </span>
                    ) : null}
                    <span className="mcp__host">
                      {item.host}:{item.port}
                    </span>
                  </label>

                  {policy.enabled ? (
                    <div className="mcp__body">
                      <div className="mcp__switches">
                        <label
                          className="mcp__switch mcp__switch--trust"
                          title="开边界后，写 systemd 单元、定时任务、提权或登录配置会先拦一下让你确认；读文件和写业务目录照旧"
                        >
                          <input
                            type="checkbox"
                            checked={!policy.fullTrust}
                            onChange={(event) =>
                              patch(item.id, { fullTrust: !event.target.checked })
                            }
                          />
                          写入边界
                        </label>
                        <label className="mcp__switch">
                          <input
                            type="checkbox"
                            checked={policy.allowExec}
                            onChange={(event) =>
                              patch(item.id, { allowExec: event.target.checked })
                            }
                          />
                          允许执行
                        </label>
                        <label className="mcp__switch">
                          <input
                            type="checkbox"
                            checked={policy.allowWrite}
                            onChange={(event) =>
                              patch(item.id, { allowWrite: event.target.checked })
                            }
                          />
                          允许写
                        </label>
                        <label className="mcp__switch">
                          <input
                            type="checkbox"
                            checked={policy.allowUpload}
                            onChange={(event) =>
                              patch(item.id, { allowUpload: event.target.checked })
                            }
                          />
                          允许上传
                        </label>
                      </div>
                      <div className="mcp__summary">
                        <span className="mcp__caption">{summarize(policy)}</span>
                        <button
                          type="button"
                          className="mcp__more"
                          onClick={() =>
                            setExpanded((prev) => ({ ...prev, [item.id]: !prev[item.id] }))
                          }
                        >
                          {expanded[item.id] ? "收起" : "细化权限"}
                        </button>
                      </div>

                      {expanded[item.id] ? (
                        <>
                          <div className="mcp__row">
                            <span className="mcp__caption">
                              限制可用工具
                              <em className="mcp__hint">
                                {policy.tools.length === 0
                                  ? "当前：全部工具，勾选后只给勾上的"
                                  : `当前：仅勾选的 ${policy.tools.length} 个`}
                              </em>
                            </span>
                            <div className="mcp__tools">
                              {TOOLS.map((tool) => (
                                <label className="mcp__tool" key={tool.id} title={tool.hint}>
                                  <input
                                    type="checkbox"
                                    checked={policy.tools.includes(tool.id)}
                                    onChange={() =>
                                      patch(item.id, { tools: toggle(policy.tools, tool.id) })
                                    }
                                  />
                                  {tool.label}
                                </label>
                              ))}
                            </div>
                          </div>

                          <div className="mcp__row">
                            <span className="mcp__caption">
                              限制可访问路径
                              <em className="mcp__hint">
                                {policy.paths.length === 0
                                  ? "留空 = 整台机器都能读写"
                                  : "只允许这些目录及其子目录"}
                              </em>
                            </span>
                            <div className="mcp__chips">
                              {policy.paths.map((path) => (
                                <span className="mcp__chip" key={path}>
                                  {path}
                                  <button
                                    type="button"
                                    className="mcp__chip-x"
                                    title="移除"
                                    onClick={() =>
                                      patch(item.id, {
                                        paths: policy.paths.filter((p) => p !== path),
                                      })
                                    }
                                  >
                                    ×
                                  </button>
                                </span>
                              ))}
                              <input
                                className="mcp__add"
                                placeholder="/var/log 后回车"
                                value={draftPath[item.id] ?? ""}
                                onChange={(event) =>
                                  setDraftPath((prev) => ({
                                    ...prev,
                                    [item.id]: event.target.value,
                                  }))
                                }
                                onKeyDown={(event) => {
                                  if (event.key !== "Enter") return;
                                  const value = (draftPath[item.id] ?? "").trim();
                                  if (!value.startsWith("/") || policy.paths.includes(value)) return;
                                  patch(item.id, { paths: [...policy.paths, value] });
                                  setDraftPath((prev) => ({ ...prev, [item.id]: "" }));
                                }}
                              />
                            </div>
                          </div>

                          <div className="mcp__row">
                            <span className="mcp__caption">
                              凭据类文件
                              <em className="mcp__hint">
                                默认挡住 SSH 私钥、authorized_keys、/etc/shadow、sudoers、
                                .pgpass、云厂商凭据。普通配置文件（.env、nginx.conf、
                                ~/.ssh/config）不受影响。
                              </em>
                            </span>
                            <label className="mcp__tool" title="这些文件一读就等于把更大的权限交出去">
                              <input
                                type="checkbox"
                                checked={policy.allowSecretFiles}
                                onChange={(event) =>
                                  patch(item.id, { allowSecretFiles: event.target.checked })
                                }
                              />
                              允许智能体读写凭据类文件
                            </label>
                          </div>

                          <div className="mcp__row">
                            <span className="mcp__caption">
                              限制可执行命令
                              <em className="mcp__hint">
                                {policy.exec.allow.length === 0
                                  ? "留空 = 任意单条命令；引号外的 ; | & > 等仍会被拒（用引号包住参数即可）"
                                  : "只允许这些命令前缀"}
                              </em>
                            </span>
                            <div className="mcp__chips">
                              {policy.exec.allow.map((command) => (
                                <span className="mcp__chip" key={command}>
                                  {command}
                                  <button
                                    type="button"
                                    className="mcp__chip-x"
                                    title="移除"
                                    onClick={() =>
                                      patchExec(item.id, {
                                        allow: policy.exec.allow.filter((c) => c !== command),
                                      })
                                    }
                                  >
                                    ×
                                  </button>
                                </span>
                              ))}
                            </div>
                            <div className="mcp__templates">
                              {templates.map((command) => (
                                <button
                                  type="button"
                                  className={`mcp__tpl${policy.exec.allow.includes(command) ? " is-on" : ""}`}
                                  key={command}
                                  onClick={() =>
                                    patchExec(item.id, {
                                      allow: toggle(policy.exec.allow, command),
                                    })
                                  }
                                >
                                  {command}
                                </button>
                              ))}
                            </div>
                            <input
                              className="mcp__add"
                              placeholder="自定义命令前缀后回车"
                              value={draftCommand[item.id] ?? ""}
                              onChange={(event) =>
                                setDraftCommand((prev) => ({
                                  ...prev,
                                  [item.id]: event.target.value,
                                }))
                              }
                              onKeyDown={(event) => {
                                if (event.key !== "Enter") return;
                                const value = (draftCommand[item.id] ?? "").trim();
                                if (!value || policy.exec.allow.includes(value)) return;
                                patchExec(item.id, {
                                  allow: [...policy.exec.allow, value],
                                });
                                setDraftCommand((prev) => ({ ...prev, [item.id]: "" }));
                              }}
                            />
                          </div>

                          <div className="mcp__row">
                            <span className="mcp__caption">执行限额</span>
                            <div className="mcp__limits">
                              <label>
                                超时
                                <input
                                  type="number"
                                  min={1}
                                  max={300}
                                  value={policy.exec.timeoutSecs}
                                  onChange={(event) =>
                                    patchExec(item.id, {
                                      timeoutSecs: clampNumber(
                                        Number(event.target.value),
                                        1,
                                        300,
                                      ),
                                    })
                                  }
                                />
                                秒
                              </label>
                              <label>
                                输出上限
                                <input
                                  type="number"
                                  min={1}
                                  max={512}
                                  value={policy.exec.maxOutputKb}
                                  onChange={(event) =>
                                    patchExec(item.id, {
                                      maxOutputKb: clampNumber(Number(event.target.value), 1, 512),
                                    })
                                  }
                                />
                                KB
                              </label>
                            </div>
                          </div>
                        </>
                      ) : null}
                    </div>
                  ) : null}
                </div>
              );
            })
          )}
        </div>
      ) : null}

      {config ? (
        <McpApprovalCard
          entries={calls}
          approvals={config.approvals}
          onChanged={() => void load()}
        />
      ) : null}

      {calls.length > 0 ? (
        <div className="mcp__calls">
          <div className="mcp__calls-head">
            <span className="mcp__caption">最近调用</span>
            <div className="mcp__calls-filter">
              <button
                type="button"
                className={`mcp__filter${onlyDenied ? "" : " is-on"}`}
                onClick={() => setOnlyDenied(false)}
              >
                全部 {calls.length}
              </button>
              <button
                type="button"
                className={`mcp__filter${onlyDenied ? " is-on" : ""}`}
                onClick={() => setOnlyDenied(true)}
              >
                仅被拒 {calls.filter((item) => item.decision === "denied").length}
              </button>
            </div>
          </div>

          <p className="mcp__calls-hint">
            「租约」是这次调用当时用来判断「能不能连」的那份文件：谁写的、写完多久。
            它防不了伪造（同一用户下任何进程都能改那个文件），所以这里给的是核对依据 ——
            某条记录的租约不是当前客户端（#{selfPid}）写的，会被标黄。
          </p>

          <ul className="mcp__calls-list">
            {shown.map((entry, index) => {
              const lease = leaseLabel(entry, selfPid, startedAt);
              const isDenied = entry.decision === "denied";
              return (
                <li className="mcp__call" key={`${entry.tsMs}-${index}`}>
                  <span
                    className="mcp__call-time"
                    title={new Date(entry.tsMs).toLocaleString()}
                  >
                    {new Date(entry.tsMs).toLocaleTimeString()}
                  </span>
                  <span className={`mcp__call-tag${isDenied ? " is-denied" : ""}`}>
                    {isDenied ? "拒绝" : "放行"}
                  </span>
                  <span className="mcp__call-conn">{entry.connection}</span>
                  <span className="mcp__call-tool">{entry.tool}</span>
                  <span
                    className={`mcp__call-lease${lease.foreign ? " is-foreign" : ""}`}
                    title={lease.hint}
                  >
                    {lease.text}
                    {lease.foreign ? " · 非本进程" : ""}
                  </span>
                  {isDenied ? (
                    <span className="mcp__call-reason" title={entry.reason}>
                      {REASON_LABEL[entry.reason] ?? entry.reason}
                    </span>
                  ) : null}
                  {entry.reason === "command_not_allowed" && !entry.args.includes("***") ? (
                    <button
                      type="button"
                      className="btn btn--ghost btn--small"
                      onClick={() => void allowDenied(entry)}
                    >
                      允许这条
                    </button>
                  ) : null}
                  <code className="mcp__call-args" title={entry.args}>
                    {entry.args || entry.tool}
                  </code>
                </li>
              );
            })}
          </ul>
        </div>
      ) : null}

      <div className="mcp__foot">
        <button type="button" className="btn btn--small" onClick={() => void copyHostConfig()}>
          复制宿主配置
        </button>
        <button type="button" className="btn btn--ghost btn--small" onClick={() => void load()}>
          刷新
        </button>
        <span className="settings__path">{auditPath}</span>
      </div>
    </section>
  );
}
