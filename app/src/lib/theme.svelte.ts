// UI-003: light / dark / system theme. The choice is remembered on this machine (localStorage) and
// applied as `data-theme` on <html>; app.html applies it before the first paint to avoid a flash.
export type Theme = "system" | "light" | "dark";
const key = "logpose-theme";

function loadTheme(): Theme {
  try {
    const t = localStorage.getItem(key);
    return t === "light" || t === "dark" ? t : "system";
  } catch {
    return "system";
  }
}

function applyTheme(t: Theme) {
  if (t === "system") delete document.documentElement.dataset.theme;
  else document.documentElement.dataset.theme = t;
  try {
    if (t === "system") localStorage.removeItem(key);
    else localStorage.setItem(key, t);
  } catch {
    // Remembering the theme is a convenience only.
  }
}

const darkQuery = typeof matchMedia === "function" ? matchMedia("(prefers-color-scheme: dark)") : null;

/** Shared by the header toggle and Settings; [`shownTheme`] is what is on screen. */
export const theme = $state({
  choice: loadTheme(),
  systemDark: darkQuery?.matches ?? false,
});
darkQuery?.addEventListener("change", (e) => (theme.systemDark = e.matches));

export function shownTheme(): "light" | "dark" {
  if (theme.choice !== "system") return theme.choice;
  return theme.systemDark ? "dark" : "light";
}

export function setTheme(t: Theme) {
  theme.choice = t;
  applyTheme(t);
}
