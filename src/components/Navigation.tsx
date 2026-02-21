import type { Screen } from "../App";
import "./Navigation.css";

interface NavigationProps {
  currentScreen: Screen;
  onNavigate: (screen: Screen) => void;
}

const NAV_ITEMS: { id: Screen; label: string; icon: string }[] = [
  { id: "record", label: "Record", icon: "⏺" },
  { id: "edit", label: "Edit", icon: "✂" },
  { id: "export", label: "Export", icon: "⬆" },
];

export default function Navigation({ currentScreen, onNavigate }: NavigationProps) {
  return (
    <nav className="nav">
      <div className="nav-brand">
        <span className="nav-logo">NSC</span>
        <span className="nav-title">NeuroScreenCaster</span>
      </div>
      <div className="nav-items">
        {NAV_ITEMS.map((item) => (
          <button
            key={item.id}
            className={`nav-item ${currentScreen === item.id ? "nav-item--active" : ""}`}
            onClick={() => onNavigate(item.id)}
          >
            <span className="nav-item-icon">{item.icon}</span>
            <span className="nav-item-label">{item.label}</span>
          </button>
        ))}
      </div>
    </nav>
  );
}
