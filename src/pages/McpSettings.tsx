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
import type {
  McpAuditEntry,
  McpConfig,
  McpConfigView,
  McpConnectionPolicy,
} from "../lib/types";

const TOOLS: { id: string; label: string; hint: string }[] = [
  { id: "list_connections", label: "列连接", hint: "查看有哪些可用服务器" },
  { id: "sftp_list", label: "列目录", hint: "只读，受可见目录限制" },
  { id: "load_snapshot", label: "负载快照", hint: "CPU / 内存 / 磁盘" },
  { id: "exec", label: "执行命令", hint: "仅白名单里的单条只读命令" },
];

function emptyPolicy(): McpConnectionPolicy {
  return {
    enabled: false,
    tools: [],
    paths: [],
    exec: { allow: [], timeoutSecs: 10, maxOutputKb: 64 },
  };
}

function normalize(view: McpConfigView): McpConfig {
  const connections: Record<string, McpConnectionPolicy> = {};
  for (const item of view.connections) {
    const existing = view.config.connections[item.id] ?? view.config.connections[item.name];
    connections[item.id] = existing
      ? { ...emptyPolicy(), ...existing, exec: { ...emptyPolicy().exec, ...existing.exec } }
      : emptyPolicy();
  }
  return { ...view.config, connections };
}

function toggle(list: string[], value: string): string[] {
  return list.includes(value) ? list.filter((item) => item !== value) : [...list, value];
}

export default function McpSettings() {
  const [config, setConfig] = useState<McpConfig | null>(null);
  const [connections, setConnections] = useState<McpConfigView["connections"]>([]);
  const [templates, setTemplates] = useState<string[]>([]);
  const [configPath, setConfigPath] = useState("");
  const [auditPath, setAuditPath] = useState("");
  const [denied, setDenied] = useState<McpAuditEntry[]>([]);
  const [error, setError] = useState<string | null>(null);
  const [saved, setSaved] = useState(true);
  const [draftPath, setDraftPath] = useState<Record<string, string>>({});
  const [draftCommand, setDraftCommand] = useState<Record<string, string>>({});
  const loaded = useRef(false);
  const saveTimer = useRef<number | null>(null);

  const load = useCallback(async () => {
    loaded.current = false;
    try {
      const [view, recent] = await Promise.all([
        call<McpConfigView>("mcp_config_get"),
        call<McpAuditEntry[]>("mcp_audit_recent"),
      ]);
      setConfig(normalize(view));
      setConnections(view.connections);
      setTemplates(view.commandTemplates);
      setConfigPath(view.configPath);
      setAuditPath(view.auditPath);
      setDenied(recent.filter((item) => item.reason !== "invalid_params"));
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
    const text = JSON.stringify(
      { mcpServers: { termharbor: { command: exe, args: ["--mcp"] } } },
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

  /** 升级上来时默认全关，给一个安全的一键起步：只开不需要路径白名单的两个只读工具。 */
  const quickStart = () => {
    setConfig((current) => {
      if (!current) return current;
      const next: Record<string, McpConnectionPolicy> = {};
      for (const item of connections) {
        next[item.id] = {
          ...(current.connections[item.id] ?? emptyPolicy()),
          enabled: true,
          tools: ["list_connections", "load_snapshot"],
        };
      }
      return { ...current, connections: next };
    });
  };

  const enabledCount = useMemo(
    () => (config ? Object.values(config.connections).filter((p) => p.enabled).length : 0),
    [config],
  );

  return (
    <section className="settings__group mcp">
      <div className="settings__label">
        <span>MCP 接入</span>
        <span className="settings__value">
          {enabledCount} 台已开放 · {saved ? "已保存" : "保存中…"}
        </span>
      </div>

      <p className="settings__notice">
        没勾选的连接，智能体看不到也连不上。改动立即生效，无需重启宿主。
        配置放产品根目录，卸载不会清掉：
        <span className="settings__path">{configPath}</span>
      </p>

      {config && enabledCount === 0 && connections.length > 0 ? (
        <p className="settings__notice mcp__quick">
          当前所有连接都未开放，智能体还看不到任何服务器。
          <button type="button" onClick={quickStart}>
            一键开放只读（列连接 + 负载快照）
          </button>
          列目录和执行命令仍需单独勾选。
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
                    <span className="mcp__host">
                      {item.host}:{item.port}
                    </span>
                  </label>

                  {policy.enabled ? (
                    <div className="mcp__body">
                      <div className="mcp__row">
                        <span className="mcp__caption">允许的工具</span>
                        <div className="mcp__tools">
                          {TOOLS.map((tool) => (
                            <label
                              className="mcp__tool"
                              key={tool.id}
                              title={tool.hint}
                            >
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
                        <span className="mcp__caption">可见目录</span>
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
                              setDraftPath((prev) => ({ ...prev, [item.id]: event.target.value }))
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

                      {policy.tools.includes("exec") ? (
                        <div className="mcp__row">
                          <span className="mcp__caption">允许的命令</span>
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
                            placeholder="自定义单条命令后回车（复合命令会被拒绝）"
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
                      ) : null}
                    </div>
                  ) : null}
                </div>
              );
            })
          )}
        </div>
      ) : null}

      {denied.length > 0 ? (
        <div className="mcp__denied">
          <div className="mcp__caption">最近被拒绝的调用</div>
          {denied.map((entry, index) => (
            <div className="mcp__denied-row" key={`${entry.tsMs}-${index}`}>
              <span className="mcp__denied-time">
                {new Date(entry.tsMs).toLocaleTimeString()}
              </span>
              <span className="mcp__denied-conn">{entry.connection}</span>
              <code className="mcp__denied-cmd">{entry.args || entry.tool}</code>
              <span className="mcp__denied-reason">{entry.reason}</span>
              {entry.reason === "command_not_allowed" ? (
                <button type="button" onClick={() => void allowDenied(entry)}>
                  允许这条
                </button>
              ) : null}
            </div>
          ))}
        </div>
      ) : null}

      <div className="mcp__foot">
        <button type="button" onClick={() => void copyHostConfig()}>
          复制宿主配置
        </button>
        <button type="button" onClick={() => void load()}>
          刷新
        </button>
        <span className="settings__path">{auditPath}</span>
      </div>
    </section>
  );
}
