import { useState } from "react";
import { getCurrentWebviewWindow } from "@tauri-apps/api/webviewWindow";
import Navigation from "./components/Navigation";
import RecordingOverlay from "./components/RecordingOverlay";
import RecordScreen from "./screens/Record";
import EditScreen from "./screens/Edit";
import ExportScreen from "./screens/Export";
import { RECORDING_OVERLAY_WINDOW_LABEL } from "./recordingOverlay";
import "./App.css";

export type Screen = "record" | "edit" | "export";

function MainApp() {
  const [screen, setScreen] = useState<Screen>("record");

  return (
    <div className="app-layout">
      <Navigation currentScreen={screen} onNavigate={setScreen} />
      <main className="app-content">
        <section className={screen === "record" ? "screen-pane" : "screen-pane screen-pane--hidden"}>
          <RecordScreen isActive={screen === "record"} />
        </section>
        {screen === "edit" && <EditScreen />}
        {screen === "export" && <ExportScreen />}
      </main>
    </div>
  );
}

export default function App() {
  if (getCurrentWebviewWindow().label === RECORDING_OVERLAY_WINDOW_LABEL) {
    return <RecordingOverlay />;
  }

  return <MainApp />;
}
