import { useState } from "react";
import Navigation from "./components/Navigation";
import RecordScreen from "./screens/Record";
import EditScreen from "./screens/Edit";
import ExportScreen from "./screens/Export";
import "./App.css";

export type Screen = "record" | "edit" | "export";

export default function App() {
  const [screen, setScreen] = useState<Screen>("record");

  return (
    <div className="app-layout">
      <Navigation currentScreen={screen} onNavigate={setScreen} />
      <main className="app-content">
        {screen === "record" && <RecordScreen />}
        {screen === "edit" && <EditScreen />}
        {screen === "export" && <ExportScreen />}
      </main>
    </div>
  );
}
