import { useCallback, useEffect, useRef, useState, type CSSProperties } from "react";

import { fromUnknown } from "../lib/errors";
import { call } from "../lib/ipc";
import type { BackupDto, Settings, TerminalTheme } from "../lib/types";
import { useSettingsStore } from "../stores/settingsStore";
import McpSettings from "./McpSettings";

/** 主题色板。终端背景色同时用于 `.term-body`，否则切浅色后四周还是黑的。 */
export const TERM_THEMES: Record<
  TerminalTheme,
  {
    background: string;
    foreground: string;
    cursor: string;
    selectionBackground: string;
  }
> = {
  dark: {
    background: "#0f1b2a",
    foreground: "#d8e2ef",
    cursor: "#5dcaa5",
    selectionBackground: "#1e3a5f",
  },
  light: {
    background: "#fafbfc",
    foreground: "#24292f",
    cursor: "#1a7f37",
    selectionBackground: "#b6d7f5",
  },
};

const FONT_SIZE_MIN = 10;
const FONT_SIZE_MAX = 24;
const SCROLLBACK_MIN = 1000;
const SCROLLBACK_MAX = 50000;

/** 滑块的已选段填充：轨道用 CSS 渐变画，位置由这个百分比决定 */
function pctStyle(value: number, min: number, max: number): CSSProperties {
  const ratio = ((value - min) / (max - min)) * 100;
  return { "--pct": `${Math.min(100, Math.max(0, ratio))}%` } as CSSProperties;
}
const COMMIT_DELAY = 400;

/** 快照时间：本地时间到分钟就够了，秒没有信息量。 */
function formatTime(ms: number): string {
  const date = new Date(ms);
  const pad = (value: number) => String(value).padStart(2, "0");
  return `${date.getFullYear()}-${pad(date.getMonth() + 1)}-${pad(date.getDate())} ${pad(
    date.getHours(),
  )}:${pad(date.getMinutes())}`;
}

function formatBytes(bytes: number): string {
  if (bytes < 1024) return `${bytes} B`;
  if (bytes < 1024 * 1024) return `${(bytes / 1024).toFixed(0)} KB`;
  return `${(bytes / 1024 / 1024).toFixed(1)} MB`;
}

export default function SettingsPage() {
  const settings = useSettingsStore((state) => state.settings);
  const saving = useSettingsStore((state) => state.saving);
  const error = useSettingsStore((state) => state.error);
  const preview = useSettingsStore((state) => state.preview);
  const commit = useSettingsStore((state) => state.commit);

  const timer = useRef<number | null>(null);

  // 组件卸载时把还没落库的改动补上，避免"调了没保存"
  useEffect(
    () => () => {
      if (timer.current !== null) {
        window.clearTimeout(timer.current);
        void commit();
      }
    },
    [commit],
  );

  /** 改一个字段：立刻反映到终端，停手 400ms 后落库。 */
  const change = (patch: Partial<Settings>) => {
    preview(patch);
    if (timer.current !== null) window.clearTimeout(timer.current);
    timer.current = window.setTimeout(() => {
      timer.current = null;
      void commit();
    }, COMMIT_DELAY);
  };

  const palette = TERM_THEMES[settings.theme];

  // ---------------------------------------------------------------- 数据备份

  const [backups, setBackups] = useState<BackupDto[]>([]);
  const [backupDir, setBackupDir] = useState<string>("");
  const [backupBusy, setBackupBusy] = useState(false);
  const [backupNotice, setBackupNotice] = useState<string | null>(null);
  const [backupError, setBackupError] = useState<string | null>(null);
  /** 正在二次确认的恢复项（文件名） */
  const [confirmRestore, setConfirmRestore] = useState<string | null>(null);

  const loadBackups = useCallback(async () => {
    try {
      const [items, dir] = await Promise.all([
        call<BackupDto[]>("backup_list"),
        call<string>("backup_dir"),
      ]);
      setBackups(items);
      setBackupDir(dir);
      setBackupError(null);
    } catch (err) {
      setBackupError(fromUnknown(err).message);
    }
  }, []);

  useEffect(() => {
    void loadBackups();
  }, [loadBackups]);

  const createBackup = async () => {
    setBackupBusy(true);
    setBackupError(null);
    try {
      await call<boolean>("backup_create");
      await loadBackups();
      setBackupNotice("已保存一份快照。");
    } catch (err) {
      setBackupError(fromUnknown(err).message);
    } finally {
      setBackupBusy(false);
    }
  };

  const restoreBackup = async (name: string) => {
    setBackupBusy(true);
    setBackupError(null);
    try {
      await call<boolean>("backup_restore", { name });
      setConfirmRestore(null);
      // 不清掉提示：用户必须看到「重启才生效」，否则会以为恢复失败了
      setBackupNotice("已安排恢复。关掉应用再打开才会生效（重新启动前会自动备份当前数据）。");
    } catch (err) {
      setBackupError(fromUnknown(err).message);
    } finally {
      setBackupBusy(false);
    }
  };

  return (
    <div className="settings-page">
      <header className="settings__head">
        <span className={`settings__status${saving ? " is-saving" : ""}`}>
          {saving ? "保存中…" : "所有改动自动保存"}
        </span>
      </header>

      {error ? <p className="settings__error">{error}</p> : null}

      <section className="settings__group">
        <div className="settings__label">
          <span>字号</span>
          <span className="settings__value">{settings.fontSize} px</span>
        </div>
        <input
          className="settings__range"
          type="range"
          min={FONT_SIZE_MIN}
          max={FONT_SIZE_MAX}
          step={1}
          value={settings.fontSize}
          style={pctStyle(settings.fontSize, FONT_SIZE_MIN, FONT_SIZE_MAX)}
          onChange={(event) => change({ fontSize: Number(event.target.value) })}
        />
        <p className="settings__hint">
          改完立刻生效。小屏笔记本建议 12–14，外接显示器可以 15 以上。
        </p>
      </section>

      <section className="settings__group">
        <div className="settings__label">
          <span>主题</span>
        </div>
        <div className="settings__themes">
          {(["dark", "light"] as const).map((theme) => (
            <button
              key={theme}
              type="button"
              className={`settings__theme ${settings.theme === theme ? "is-active" : ""}`}
              onClick={() => change({ theme })}
            >
              <span
                className="settings__swatch"
                style={{
                  background: TERM_THEMES[theme].background,
                  color: TERM_THEMES[theme].foreground,
                }}
              >
                Aa
              </span>
              {theme === "dark" ? "深色" : "浅色"}
            </button>
          ))}
        </div>
        <p className="settings__hint">
          浅色在强光环境下更省眼；深色适合长时间盯屏。
        </p>
      </section>

      <section className="settings__group">
        <div className="settings__label">
          <span>回滚缓冲</span>
          <span className="settings__value">{settings.scrollback.toLocaleString()} 行</span>
        </div>
        <input
          className="settings__range"
          type="range"
          min={SCROLLBACK_MIN}
          max={SCROLLBACK_MAX}
          step={1000}
          value={settings.scrollback}
          style={pctStyle(settings.scrollback, SCROLLBACK_MIN, SCROLLBACK_MAX)}
          onChange={(event) => change({ scrollback: Number(event.target.value) })}
        />
        <p className="settings__hint">
          终端最多记住多少行历史。调大能翻更久之前的输出，但每个会话都会多占内存。
        </p>
      </section>

      <section className="settings__group">
        <div className="settings__label">
          <span>数据备份</span>
          <button
            type="button"
            className="btn btn--small"
            disabled={backupBusy}
            onClick={() => void createBackup()}
          >
            {backupBusy ? "处理中…" : "立即备份"}
          </button>
        </div>
        <p className="settings__hint">
          每次启动都会自动留一份快照，保留最近 5 份。装新版本时如果勾了「删除应用数据」，
          连接配置会被清掉 —— 这时候可以在这里恢复。
        </p>

        {backupError ? <p className="settings__error">{backupError}</p> : null}
        {backupNotice ? <p className="settings__notice">{backupNotice}</p> : null}

        {backups.length === 0 ? (
          <p className="settings__hint">还没有快照。</p>
        ) : (
          <ul className="settings__backups">
            {backups.map((item) => (
              <li key={item.name} className="settings__backup">
                <span className="settings__backup-time">{formatTime(item.createdMs)}</span>
                <span className="settings__backup-meta">
                  {item.connections === null ? "连接数未知" : `${item.connections} 台服务器`}
                  {" · "}
                  {formatBytes(item.bytes)}
                </span>
                {confirmRestore === item.name ? (
                  <span className="settings__backup-actions">
                    <button
                      type="button"
                      className="btn btn--small btn--danger"
                      disabled={backupBusy}
                      onClick={() => void restoreBackup(item.name)}
                    >
                      确认恢复
                    </button>
                    <button
                      type="button"
                      className="btn btn--small"
                      onClick={() => setConfirmRestore(null)}
                    >
                      取消
                    </button>
                  </span>
                ) : (
                  <button
                    type="button"
                    className="btn btn--small"
                    disabled={backupBusy}
                    onClick={() => {
                      setBackupNotice(null);
                      setConfirmRestore(item.name);
                    }}
                  >
                    恢复
                  </button>
                )}
              </li>
            ))}
          </ul>
        )}

        {backupDir ? (
          <p className="settings__hint">
            快照目录：<span className="settings__path">{backupDir}</span>
          </p>
        ) : null}
      </section>

      <section className="settings__group">
        <div className="settings__label">
          <span>预览</span>
        </div>
        <div
          className="settings__preview"
          style={{
            background: palette.background,
            color: palette.foreground,
            fontSize: `${settings.fontSize}px`,
          }}
        >
          <div>root@server:~# df -h</div>
          <div>Filesystem      Size  Used Avail Use% Mounted on</div>
          <div>/dev/vda1        40G   12G   26G  32% /</div>
          <div style={{ color: palette.cursor }}>root@server:~# █</div>
        </div>
      </section>

      <McpSettings />
    </div>
  );
}
