import { lazy, Suspense, useEffect } from "react";

import { usePing } from "./hooks/usePing";
import ConnectionsPage from "./pages/ConnectionsPage";
import SelfCheckPage from "./pages/SelfCheckPage";
import AboutPage from "./pages/AboutPage";
import SettingsPage from "./pages/SettingsPage";
import TerminalPage from "./pages/TerminalPage";
import { useSessionStore } from "./stores/sessionStore";
import { useSettingsStore } from "./stores/settingsStore";
import { useUiStore, type PageId } from "./stores/uiStore";

interface NavItem {
  id: PageId;
  label: string;
}

const NAV: NavItem[] = [
  { id: "selfcheck", label: "自检" },
  { id: "connections", label: "连接" },
  { id: "terminal", label: "终端" },
  { id: "settings", label: "设置" },
  { id: "about", label: "关于" },
];

// S3 的输入法取证页只在开发构建里出现。用 import.meta.env.DEV 做守卫，
// 生产构建时 Rollup 会把整个动态 import 分支判死，xterm 不会被包进正式产物。
const ImeTestPage = import.meta.env.DEV ? lazy(() => import("./pages/ImeTestPage")) : null;

if (import.meta.env.DEV) {
  NAV.push({ id: "ime", label: "输入法测试" });
}

const TITLES: Record<PageId, string> = {
  selfcheck: "自检",
  connections: "连接管理",
  terminal: "终端会话",
  settings: "设置",
  about: "关于",
  ime: "输入法测试",
};

function BrandMark() {
  return (
    <svg className="brand__mark" viewBox="0 0 32 32" aria-hidden="true">
      <rect width="32" height="32" rx="8" fill="#0F2A47" />
      <path
        d="M11 9.5 L19.5 16 L11 22.5"
        fill="none"
        stroke="#5DCAA5"
        strokeWidth="2.6"
        strokeLinecap="round"
        strokeLinejoin="round"
      />
      <rect x="21" y="19.4" width="5" height="2.6" rx="1.3" fill="#B5D4F4" />
    </svg>
  );
}

export default function App() {
  const { activePage, setActivePage } = useUiStore();
  const sessionCount = useSessionStore((state) => state.tabs.length);
  const { pong, error, loading, refresh } = usePing();

  const loadSettings = useSettingsStore((state) => state.load);

  // 设置要在终端建立会话之前就位，否则第一个标签会用默认字号
  useEffect(() => {
    void loadSettings();
  }, [loadSettings]);

  const statusText = error ? "后端异常" : pong ? (pong.locked ? "未解锁" : "就绪") : "连接中";
  const statusTone = error || pong?.locked ? "warn" : pong ? "ok" : "idle";

  return (
    <div className="shell">
      <aside className="sidebar">
        <div className="brand">
          <BrandMark />
          <div className="brand__text">
            <strong>TermHarbor</strong>
            <span>终端港</span>
          </div>
        </div>

        <nav className="nav">
          {NAV.map((item) => (
            <button
              key={item.id}
              type="button"
              className={`nav__item ${activePage === item.id ? "is-active" : ""}`}
              onClick={() => setActivePage(item.id)}
            >
              {item.label}
            </button>
          ))}
        </nav>

        <div className="sidebar__foot">
          <span className={`dot dot--${statusTone}`} />
          <span>{statusText}</span>
        </div>
      </aside>

      <main className="main">
        <header className="topbar">
          <h1>{TITLES[activePage]}</h1>
          <span className="topbar__meta">
            {pong ? `v${pong.version} · schema v${pong.schemaVersion}` : loading ? "…" : "—"}
          </span>
        </header>

        <section className="content">
          {activePage === "selfcheck" && (
            <SelfCheckPage pong={pong} error={error} loading={loading} refresh={refresh} />
          )}

          {activePage === "connections" && <ConnectionsPage />}

          {/* 终端页常驻挂载：切到别的页面时只是隐藏，否则所有会话都会被销毁 */}
          <div className="content__slot" hidden={activePage !== "terminal"}>
            <TerminalPage />
          </div>

          {activePage === "ime" && ImeTestPage ? (
            <Suspense fallback={<div className="skeleton">正在加载终端组件…</div>}>
              <ImeTestPage />
            </Suspense>
          ) : null}

          {activePage === "settings" && <SettingsPage />}

          {activePage === "about" && <AboutPage pong={pong} />}
        </section>

        <footer className="statusbar">
          <span>会话 {sessionCount}</span>
          <span className="statusbar__sep" />
          <span>密钥 {pong?.keySource ?? "—"}</span>
          <span className="statusbar__spacer" />
          <span>{error ? error.code : "IPC 就绪"}</span>
        </footer>
      </main>
    </div>
  );
}
