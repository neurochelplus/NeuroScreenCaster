import { useCallback, useEffect, useState, type ReactElement } from "react";
import { getCurrentWindow } from "@tauri-apps/api/window";
import type { Screen } from "../App";
import "./Navigation.css";

interface NavigationProps {
  currentScreen: Screen;
  onNavigate: (screen: Screen) => void;
  onRequestClose?: () => void;
}

interface NavItem {
  id: Screen;
  label: string;
  hint: string;
  icon: ReactElement;
}

function RecordIcon() {
  return (
    <svg viewBox="0 0 20 20" fill="none" aria-hidden="true">
      <rect x="3.5" y="3.5" width="13" height="13" rx="3.5" stroke="currentColor" strokeWidth="1.5" />
      <circle cx="10" cy="10" r="2.8" fill="currentColor" />
    </svg>
  );
}

function EditIcon() {
  return (
    <svg viewBox="0 0 20 20" fill="none" aria-hidden="true">
      <path
        d="M4 13.5L13.9 3.6a1.6 1.6 0 0 1 2.3 0l.2.2a1.6 1.6 0 0 1 0 2.3L6.5 16H4v-2.5Z"
        stroke="currentColor"
        strokeWidth="1.5"
        strokeLinejoin="round"
      />
      <path d="M11.9 5.6l2.5 2.5" stroke="currentColor" strokeWidth="1.5" strokeLinecap="round" />
    </svg>
  );
}

function ExportIcon() {
  return (
    <svg viewBox="0 0 20 20" fill="none" aria-hidden="true">
      <path d="M10 3.8v8.1" stroke="currentColor" strokeWidth="1.5" strokeLinecap="round" />
      <path d="m6.8 8.7 3.2 3.2 3.2-3.2" stroke="currentColor" strokeWidth="1.5" strokeLinecap="round" />
      <rect x="4" y="13.1" width="12" height="2.9" rx="1.45" stroke="currentColor" strokeWidth="1.5" />
    </svg>
  );
}

function MinimizeIcon() {
  return (
    <svg viewBox="0 0 20 20" fill="none" aria-hidden="true">
      <path d="M4.5 10.5h11" stroke="currentColor" strokeWidth="1.6" strokeLinecap="round" />
    </svg>
  );
}

function MaximizeIcon() {
  return (
    <svg viewBox="0 0 20 20" fill="none" aria-hidden="true">
      <rect x="5.2" y="5.2" width="9.6" height="9.6" rx="1.5" stroke="currentColor" strokeWidth="1.5" />
    </svg>
  );
}

function RestoreIcon() {
  return (
    <svg viewBox="0 0 20 20" fill="none" aria-hidden="true">
      <rect x="7.4" y="5.1" width="7.5" height="7.5" rx="1.2" stroke="currentColor" strokeWidth="1.4" />
      <path
        d="M5.1 7.6V13a1.8 1.8 0 0 0 1.8 1.8h5.4"
        stroke="currentColor"
        strokeWidth="1.4"
        strokeLinecap="round"
      />
    </svg>
  );
}

function CloseIcon() {
  return (
    <svg viewBox="0 0 20 20" fill="none" aria-hidden="true">
      <path d="m6 6 8 8M14 6l-8 8" stroke="currentColor" strokeWidth="1.6" strokeLinecap="round" />
    </svg>
  );
}

const NAV_ITEMS: NavItem[] = [
  { id: "record", label: "Record", hint: "Capture", icon: <RecordIcon /> },
  { id: "edit", label: "Edit", hint: "Timeline", icon: <EditIcon /> },
  { id: "export", label: "Export", hint: "Render", icon: <ExportIcon /> },
];

export default function Navigation({ currentScreen, onNavigate, onRequestClose }: NavigationProps) {
  const appWindow = getCurrentWindow();
  const [isMaximized, setIsMaximized] = useState(false);

  const syncMaximizedState = useCallback(async () => {
    try {
      const maximized = await appWindow.isMaximized();
      setIsMaximized(maximized);
    } catch {
      // Window controls are best-effort in browser fallback.
    }
  }, [appWindow]);

  useEffect(() => {
    void syncMaximizedState();
    const unlistenPromise = appWindow.listen("tauri://resize", () => {
      void syncMaximizedState();
    });
    return () => {
      void unlistenPromise.then((unlisten) => {
        unlisten();
      });
    };
  }, [appWindow, syncMaximizedState]);

  const handleMinimize = useCallback(() => {
    void appWindow.minimize();
  }, [appWindow]);

  const handleToggleMaximize = useCallback(() => {
    void appWindow.toggleMaximize().then(() => {
      void syncMaximizedState();
    });
  }, [appWindow, syncMaximizedState]);

  const handleClose = useCallback(() => {
    if (onRequestClose) {
      onRequestClose();
      return;
    }
    void appWindow.close();
  }, [appWindow, onRequestClose]);

  return (
    <nav className="nav">
      <div className="nav-shell">
        <div
          className="nav-brand"
          aria-label="NeuroScreenCaster"
          data-tauri-drag-region
          onDoubleClick={() => handleToggleMaximize()}
        >
          <div className="nav-brand-mark">
            <img className="nav-brand-mark-img" src="/favicon.png" alt="NeuroScreenCaster logo" />
          </div>
          <div className="nav-brand-copy">
            <span className="nav-title">NeuroScreenCaster</span>
            <span className="nav-subtitle">Operator Grid</span>
          </div>
        </div>

        <div className="nav-items" role="tablist" aria-label="Workspace">
          {NAV_ITEMS.map((item) => {
            const isActive = currentScreen === item.id;
            return (
              <button
                key={item.id}
                className={`nav-item ${isActive ? "nav-item--active" : ""}`}
                onClick={() => onNavigate(item.id)}
                role="tab"
                aria-selected={isActive}
                aria-label={item.label}
              >
                <span className="nav-item-icon">{item.icon}</span>
                <span className="nav-item-copy">
                  <span className="nav-item-label">{item.label}</span>
                  <span className="nav-item-hint">{item.hint}</span>
                </span>
              </button>
            );
          })}
        </div>

        <div className="nav-right">
          <div className="nav-status">
            <span className="nav-status-dot" />
            <span>Workspace Ready</span>
          </div>

          <div className="window-controls">
            <button
              className="window-control-btn"
              onClick={handleMinimize}
              aria-label="Minimize window"
              title="Minimize"
            >
              <MinimizeIcon />
            </button>
            <button
              className="window-control-btn"
              onClick={handleToggleMaximize}
              aria-label={isMaximized ? "Restore window" : "Maximize window"}
              title={isMaximized ? "Restore" : "Maximize"}
            >
              {isMaximized ? <RestoreIcon /> : <MaximizeIcon />}
            </button>
            <button
              className="window-control-btn window-control-btn--close"
              onClick={handleClose}
              aria-label="Close window"
              title="Close"
            >
              <CloseIcon />
            </button>
          </div>
        </div>
      </div>
    </nav>
  );
}
