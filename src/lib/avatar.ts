// Avatar helpers. Colour is derived from the Ed25519 SPK so the same
// sender renders with the same hue across pages and sessions.

const LIGHT_BG_SAT = 55;
const LIGHT_BG_LIGHT = 65;
const LIGHT_FG_SAT = 55;
const LIGHT_FG_LIGHT = 25;

const DARK_BG_SAT = 35;
const DARK_BG_LIGHT = 32;
const DARK_FG_SAT = 70;
const DARK_FG_LIGHT = 80;

// Cached at first call; switching the OS theme mid-session needs a reload.
let darkPrefCache: boolean | null = null;
function prefersDark(): boolean {
  if (darkPrefCache !== null) return darkPrefCache;
  if (typeof window === "undefined" || !window.matchMedia) {
    darkPrefCache = false;
    return darkPrefCache;
  }
  darkPrefCache = window.matchMedia("(prefers-color-scheme: dark)").matches;
  return darkPrefCache;
}

// Stable HSL hue from the first 24 bits of the SPK; 220 fallback on bad input.
export function avatarHue(spkHex: string | null | undefined): number {
  if (!spkHex || spkHex.length < 6) return 220;
  const slice = spkHex.slice(0, 6);
  const n = parseInt(slice, 16);
  if (Number.isNaN(n)) return 220;
  return n % 360;
}

export function avatarBackground(spkHex: string | null | undefined): string {
  const h = avatarHue(spkHex);
  if (prefersDark()) {
    return `hsl(${h}, ${DARK_BG_SAT}%, ${DARK_BG_LIGHT}%)`;
  }
  return `hsl(${h}, ${LIGHT_BG_SAT}%, ${LIGHT_BG_LIGHT}%)`;
}

export function avatarForeground(spkHex: string | null | undefined): string {
  const h = avatarHue(spkHex);
  if (prefersDark()) {
    return `hsl(${h}, ${DARK_FG_SAT}%, ${DARK_FG_LIGHT}%)`;
  }
  return `hsl(${h}, ${LIGHT_FG_SAT}%, ${LIGHT_FG_LIGHT}%)`;
}

// First letter of the display name when present; otherwise first two hex
// chars of the SPK so unpinned senders still get a stable monogram.
export function avatarInitials(
  displayName: string | null | undefined,
  spkHex: string | null | undefined,
): string {
  const name = (displayName ?? "").trim();
  if (name.length > 0) {
    return name.charAt(0).toUpperCase();
  }
  if (spkHex && spkHex.length >= 2) {
    return spkHex.slice(0, 2).toUpperCase();
  }
  return "?";
}
