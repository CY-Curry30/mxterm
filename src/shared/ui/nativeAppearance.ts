export interface NativeAppearance {
  panel: number;
  text: number;
  muted: number;
  line: number;
  primary: number;
  danger: number;
  chrome: number;
  chrome_line: number;
  active: number;
  hover: number;
  success: number;
  reduced_motion: boolean;
}

/** Snapshot the resolved app tokens for native windows outside the WebView. */
export function readNativeAppearance(): NativeAppearance | undefined {
  const root = document.querySelector(".app-shell");
  if (!root) return undefined;
  const styles = getComputedStyle(root);
  const canvas = document.createElement("canvas");
  canvas.width = canvas.height = 1;
  const context = canvas.getContext("2d", { willReadFrequently: true });
  if (!context) return undefined;
  const color = (token: string, background = "--mx-bg") => {
    context.clearRect(0, 0, 1, 1);
    context.fillStyle = styles.getPropertyValue("--mx-bg").trim();
    context.fillRect(0, 0, 1, 1);
    if (background !== "--mx-bg") {
      context.fillStyle = styles.getPropertyValue(background).trim();
      context.fillRect(0, 0, 1, 1);
    }
    context.fillStyle = styles.getPropertyValue(token).trim();
    context.fillRect(0, 0, 1, 1);
    const [r, g, b] = context.getImageData(0, 0, 1, 1).data;
    return r | (g << 8) | (b << 16);
  };
  return {
    panel: color("--mx-panel"), text: color("--mx-text"),
    muted: color("--mx-muted"), line: color("--mx-line"),
    primary: color("--mx-primary"), danger: color("--mx-danger"),
    chrome: color("--mx-chrome-fill"), chrome_line: color("--mx-chrome-line", "--mx-chrome-fill"),
    active: color("--mx-panel"), hover: color("--mx-active", "--mx-panel"),
    success: color("--mx-success"),
    reduced_motion: matchMedia("(prefers-reduced-motion: reduce)").matches,
  };
}
