import { useEffect, useRef } from "react";

import type { Settings, TerminalTheme } from "../lib/types";
import { useSettingsStore } from "../stores/settingsStore";

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
const COMMIT_DELAY = 400;

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

  return (
    <div className="settings-page">
      <header className="settings__head">
        <h2>设置</h2>
        <span className="settings__status">
          {saving ? "保存中…" : "已保存"}
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
          onChange={(event) => change({ scrollback: Number(event.target.value) })}
        />
        <p className="settings__hint">
          终端最多记住多少行历史。调大能翻更久之前的输出，但每个会话都会多占内存。
        </p>
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
    </div>
  );
}
