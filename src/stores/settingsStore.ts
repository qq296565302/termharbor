import { create } from "zustand";

import { fromUnknown } from "../lib/errors";
import { call } from "../lib/ipc";
import { applyTheme } from "../lib/theme";
import type { Settings } from "../lib/types";

/** 与后端默认值保持一致；后端会对越界值做 clamp。 */
export const DEFAULT_SETTINGS: Settings = {
  fontSize: 13,
  theme: "dark",
  scrollback: 5000,
  uiTheme: "system",
  backgroundImage: "",
  backgroundOpacity: 40,
  commandBar: true,
  autoPack: true,
  packThreshold: 5,
  monitorInterfaces: "",
};

interface SettingsState {
  settings: Settings;
  loaded: boolean;
  saving: boolean;
  error: string | null;
  /** 启动时拉一次。失败不阻断使用，退回默认值。 */
  load: () => Promise<void>;
  /** 只改内存，立刻反映到终端；落库由调用方决定时机。 */
  preview: (patch: Partial<Settings>) => void;
  /** 把当前内存里的设置写进库。 */
  commit: () => Promise<void>;
}

/**
 * 设置为什么用「preview + commit」两步：
 *
 * 拖字号滑块时终端必须立刻跟着变，否则用户得松手才知道效果。
 * 但拖动会连续触发几十次，每次都打一次 IPC 落库既浪费又会让设置表频繁写。
 * 所以界面连续调 `preview`，调用方用防抖在停手后再 `commit` 一次。
 */
export const useSettingsStore = create<SettingsState>((set, get) => ({
  settings: DEFAULT_SETTINGS,
  loaded: false,
  saving: false,
  error: null,

  load: async () => {
    try {
      const settings = await call<Settings>("settings_get");
      // 库里读到的外观覆盖掉首帧用的缓存值。缓存只是猜测，这里才是事实。
      applyTheme(settings.uiTheme);
      set({ settings, loaded: true, error: null });
    } catch (err) {
      // 读不到就用默认值，别让设置页变成错误页
      set({ loaded: true, error: fromUnknown(err).message });
    }
  },

  preview: (patch) => {
    // 只有外观需要立刻落地：其余字段（字号、回滚行数）由终端自己订阅。
    // 加个判断是因为 preview 会被滑块连续调用几十次，没必要每次都写一遍属性。
    if (patch.uiTheme) applyTheme(patch.uiTheme);
    set((state) => ({ settings: { ...state.settings, ...patch } }));
  },

  commit: async () => {
    const current = get().settings;
    set({ saving: true, error: null });
    try {
      // 后端会 clamp 后返回，用返回值覆盖本地，避免界面显示越界值
      const saved = await call<Settings>("settings_update", { settings: current });
      // 后端可能把越界值改掉，以返回值为准
      applyTheme(saved.uiTheme);
      set({ settings: saved, saving: false });
    } catch (err) {
      set({ saving: false, error: fromUnknown(err).message });
    }
  },
}));
