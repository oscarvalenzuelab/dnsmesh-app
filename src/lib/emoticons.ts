// Classic MSN/Trillian/ICQ shortcodes → unicode. Display-only — the
// raw `:)` text stays on the wire so recipients on other clients see
// what was typed. The composer picker inserts the unicode glyph
// directly because there's no point storing both.

export const SHORTCODES: ReadonlyArray<readonly [string, string]> = [
  // Faces
  [":)", "🙂"],
  [":-)", "🙂"],
  [":(", "🙁"],
  [":-(", "🙁"],
  [":D", "😀"],
  [":-D", "😀"],
  ["xD", "😆"],
  ["XD", "😆"],
  [":P", "😛"],
  [":-P", "😛"],
  [":p", "😛"],
  [";)", "😉"],
  [";-)", "😉"],
  [":o", "😮"],
  [":O", "😮"],
  [":-o", "😮"],
  [":-O", "😮"],
  [":|", "😐"],
  [":-|", "😐"],
  [":/", "😕"],
  [":-/", "😕"],
  [":'(", "😢"],
  [":*", "😘"],
  [":-*", "😘"],
  ["B)", "😎"],
  ["B-)", "😎"],
  // Symbols
  ["<3", "❤️"],
  ["</3", "💔"],
  ["(y)", "👍"],
  ["(Y)", "👍"],
  ["(n)", "👎"],
  ["(N)", "👎"],
  ["(*)", "⭐"],
  ["o/", "👋"],
  ["\\o", "👋"],
];

// Picker palette — small curated grid of glyphs that cover the common
// reactions without overwhelming a single popover.
export const PICKER_EMOJI: ReadonlyArray<string> = [
  "🙂", "😀", "😉", "😆", "😅", "😂", "🤣",
  "😊", "😍", "😘", "😎", "🤔", "😐", "😕",
  "🙁", "😢", "😭", "😡", "🤯", "😱", "🥳",
  "❤️", "💔", "👍", "👎", "👏", "🙏", "👋",
  "🔥", "✨", "⭐", "💯", "🎉", "✅", "❌",
];

// Escape for use inside a RegExp character class / pattern body.
function escapeRegex(s: string): string {
  return s.replace(/[.*+?^${}()|[\]\\]/g, "\\$&");
}

// Sort longest first so `:-)` wins over `:)` and we don't strand a
// stray dash on the longer-token replacements.
const COMPILED = (() => {
  const sorted = [...SHORTCODES].sort((a, b) => b[0].length - a[0].length);
  const map = new Map<string, string>();
  for (const [k, v] of sorted) map.set(k, v);
  const alternation = sorted.map(([k]) => escapeRegex(k)).join("|");
  // Boundary rule: each shortcode must be flanked by start/end of
  // string, whitespace, or sentence punctuation. Avoids replacing the
  // tail of e.g. `https://x.y(:)`.
  const pattern = new RegExp(
    `(^|[\\s.,!?;:])(${alternation})(?=$|[\\s.,!?;:])`,
    "g",
  );
  return { map, pattern };
})();

// Display-time conversion. Safe to call on every render — operates on
// short strings and the regex is precompiled.
export function renderEmoticons(text: string): string {
  return text.replace(COMPILED.pattern, (_full, lead: string, code: string) => {
    const replacement = COMPILED.map.get(code) ?? code;
    return lead + replacement;
  });
}
