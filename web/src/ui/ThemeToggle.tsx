// Two-state color-scheme toggle (modern-web-guidance "dark-mode-toggle"):
// the system default, or the opposite of what is shown now, stored literally.
import { useEffect, useState } from "react";
import { Icon } from "./icons";

const KEY = "color-scheme";

function meta(): HTMLMetaElement | null {
  return document.querySelector('meta[name="color-scheme"]');
}

function rendered(): "light" | "dark" {
  const stored = meta()?.content;
  if (stored === "light" || stored === "dark") return stored;
  return window.matchMedia("(prefers-color-scheme: dark)").matches ? "dark" : "light";
}

export function ThemeToggle() {
  const [scheme, setScheme] = useState<"light" | "dark">(() => rendered());

  useEffect(() => {
    const mq = window.matchMedia("(prefers-color-scheme: dark)");
    const sync = () => setScheme(rendered());
    const onStorage = (e: StorageEvent) => {
      if (e.key !== KEY) return;
      const m = meta();
      if (m) m.content = e.newValue ?? "light dark";
      sync();
    };
    mq.addEventListener("change", sync);
    window.addEventListener("storage", onStorage);
    return () => {
      mq.removeEventListener("change", sync);
      window.removeEventListener("storage", onStorage);
    };
  }, []);

  const toggle = () => {
    const target = rendered() === "dark" ? "light" : "dark";
    const system = window.matchMedia("(prefers-color-scheme: dark)").matches ? "dark" : "light";
    const m = meta();
    try {
      if (target === system) localStorage.removeItem(KEY);
      else localStorage.setItem(KEY, target);
    } catch {
      // Storage blocked: the change still applies to this page.
    }
    if (m) m.content = target === system ? "light dark" : target;
    setScheme(target);
  };

  const label = scheme === "dark" ? "Cambiar a tema claro" : "Cambiar a tema oscuro";
  return (
    <button type="button" className="icon-btn theme" onClick={toggle} aria-label={label} title={label}>
      <Icon name={scheme === "dark" ? "sun" : "moon"} />
    </button>
  );
}
