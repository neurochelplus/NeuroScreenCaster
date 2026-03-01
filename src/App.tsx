import { useCallback, useEffect, useRef, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { getCurrentWindow } from "@tauri-apps/api/window";
import { WebviewWindow, getCurrentWebviewWindow } from "@tauri-apps/api/webviewWindow";
import Navigation from "./components/Navigation";
import RecordingOverlay from "./components/RecordingOverlay";
import RecordScreen from "./screens/Record";
import EditScreen from "./screens/Edit";
import ExportScreen from "./screens/Export";
import { RECORDING_OVERLAY_WINDOW_LABEL } from "./recordingOverlay";
import "./App.css";

export type Screen = "record" | "edit" | "export";

type PendingAction =
  | { kind: "navigate"; target: Screen }
  | { kind: "close" }
  | null;

function MainApp() {
  const appWindow = getCurrentWindow();
  const [screen, setScreen] = useState<Screen>("record");
  const [isEditDirty, setIsEditDirty] = useState(false);
  const [pendingAction, setPendingAction] = useState<PendingAction>(null);
  const [showUnsavedPrompt, setShowUnsavedPrompt] = useState(false);
  const [isHandlingUnsavedPrompt, setIsHandlingUnsavedPrompt] = useState(false);
  const editSaveHandlerRef = useRef<null | (() => Promise<boolean>)>(null);
  const bypassCloseGuardRef = useRef(false);

  const closeAuxiliaryWindows = useCallback(async () => {
    try {
      const overlayWindow = await WebviewWindow.getByLabel(RECORDING_OVERLAY_WINDOW_LABEL);
      if (overlayWindow) {
        await overlayWindow.close();
      }
    } catch {
      // Best-effort close for auxiliary windows.
    }
  }, []);

  const shouldPromptForAction = useCallback(
    (action: PendingAction): boolean => {
      if (!action || screen !== "edit" || !isEditDirty) {
        return false;
      }
      if (action.kind === "navigate" && action.target === "edit") {
        return false;
      }
      return true;
    },
    [isEditDirty, screen]
  );

  const executePendingAction = useCallback(
    (action: PendingAction) => {
      if (!action) {
        return;
      }
      if (action.kind === "navigate") {
        setScreen(action.target);
        setIsEditDirty(false);
        return;
      }
      const closeTask = async () => {
        await closeAuxiliaryWindows();
        try {
          await invoke("exit_application");
        } catch {
          bypassCloseGuardRef.current = true;
          try {
            await appWindow.close();
          } catch {
            await appWindow.destroy();
          }
        }
      };
      void closeTask();
    },
    [appWindow, closeAuxiliaryWindows]
  );

  const requestNavigate = useCallback(
    (target: Screen) => {
      const action: PendingAction = { kind: "navigate", target };
      if (shouldPromptForAction(action)) {
        setPendingAction(action);
        setShowUnsavedPrompt(true);
        return;
      }
      setScreen(target);
    },
    [shouldPromptForAction]
  );

  const requestClose = useCallback(() => {
    const action: PendingAction = { kind: "close" };
    if (shouldPromptForAction(action)) {
      setPendingAction(action);
      setShowUnsavedPrompt(true);
      return;
    }
    void executePendingAction(action);
  }, [executePendingAction, shouldPromptForAction]);

  useEffect(() => {
    const unlisten = appWindow.onCloseRequested((event) => {
      if (bypassCloseGuardRef.current) {
        bypassCloseGuardRef.current = false;
        return;
      }
      event.preventDefault();
      requestClose();
    });

    return () => {
      void unlisten.then((dispose) => {
        dispose();
      });
    };
  }, [appWindow, requestClose]);

  const handleUnsavedChoiceNo = useCallback(() => {
    const action = pendingAction;
    setShowUnsavedPrompt(false);
    setPendingAction(null);
    executePendingAction(action);
  }, [executePendingAction, pendingAction]);

  const handleUnsavedChoiceCancel = useCallback(() => {
    setShowUnsavedPrompt(false);
    setPendingAction(null);
  }, []);

  const handleUnsavedChoiceYes = useCallback(async () => {
    const action = pendingAction;
    setIsHandlingUnsavedPrompt(true);
    try {
      const saveHandler = editSaveHandlerRef.current;
      if (saveHandler) {
        const saved = await saveHandler();
        if (!saved) {
          return;
        }
      }
      setShowUnsavedPrompt(false);
      setPendingAction(null);
      executePendingAction(action);
    } finally {
      setIsHandlingUnsavedPrompt(false);
    }
  }, [executePendingAction, pendingAction]);

  return (
    <div className="app-layout">
      <Navigation currentScreen={screen} onNavigate={requestNavigate} onRequestClose={requestClose} />
      <main className={`app-content app-content--${screen}`}>
        <div className={`app-content-frame app-content-frame--${screen}`}>
          <section className={screen === "record" ? "screen-pane" : "screen-pane screen-pane--hidden"}>
            <RecordScreen isActive={screen === "record"} />
          </section>
          {screen === "edit" && (
            <EditScreen
              onDirtyChange={setIsEditDirty}
              onSaveHandlerChange={(handler) => {
                editSaveHandlerRef.current = handler;
              }}
            />
          )}
          {screen === "export" && <ExportScreen />}
        </div>
      </main>

      {showUnsavedPrompt && (
        <div className="app-modal-backdrop" role="presentation">
          <div className="app-modal" role="dialog" aria-modal="true" aria-labelledby="unsaved-title">
            <h3 id="unsaved-title">Save changes?</h3>
            <p>You have unsaved changes in the editor.</p>
            <div className="app-modal-actions">
              <button
                className="btn-primary"
                onClick={() => void handleUnsavedChoiceYes()}
                disabled={isHandlingUnsavedPrompt}
              >Yes</button>
              <button
                className="btn-ghost"
                onClick={handleUnsavedChoiceNo}
                disabled={isHandlingUnsavedPrompt}
              >No</button>
              <button
                className="btn-ghost"
                onClick={handleUnsavedChoiceCancel}
                disabled={isHandlingUnsavedPrompt}
              >Cancel</button>
            </div>
          </div>
        </div>
      )}
    </div>
  );
}

export default function App() {
  if (getCurrentWebviewWindow().label === RECORDING_OVERLAY_WINDOW_LABEL) {
    return <RecordingOverlay />;
  }

  return <MainApp />;
}

