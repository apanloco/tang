// The name a session is saved as, in the page's mock engine: the controller's rule (`session::plain_file_name`), which the fixture `filenames.json` pins it to,
// name by name. A name is a name and not a path: the design (docs/web-ui.md §8) says a page never sends a path, and a server that took `dir.join(name)` would let an
// absolute one replace the directory.

/** The names that Windows keeps for devices: a file cannot be called one of them there, with any extension after it. */
const WINDOWS_DEVICE_NAMES = new Set(["con", "prn", "aux", "nul", ...[1, 2, 3, 4, 5, 6, 7, 8, 9].flatMap((n) => [`com${String(n)}`, `lpt${String(n)}`])]);

/** The most bytes a file name has on the file systems that matter. */
const MAX_FILE_NAME_BYTES = 255;

/** Characters that a file cannot have on one system or another, and the controls (U+0000 to U+001F and U+007F to U+009F, which Rust's `is_control` says too). */
const hasForbidden = (name: string): boolean =>
  Array.from(name).some((character) => {
    const code = character.codePointAt(0) ?? 0;
    return code <= 0x1f || (code >= 0x7f && code <= 0x9f) || ':*?"<>|'.includes(character);
  });

/** The white space that Rust's `trim` takes off the ends: the Unicode property, which is not JavaScript's (it has U+0085 and not U+FEFF). */
const WHITE_SPACE_AT_THE_ENDS = /^[\t-\r \u0085\u00a0\u1680\u2000-\u200a\u2028\u2029\u202f\u205f\u3000]+|[\t-\r \u0085\u00a0\u1680\u2000-\u200a\u2028\u2029\u202f\u205f\u3000]+$/gu;

/**
 * The file that a session is saved as when it is given the name `given`, if that is a plain file name; or the words for what is wrong with it (the controller's).
 * The spaces round it go, `.toml` is added unless it ends in it, and what is left is a name and not a place.
 */
export function sessionFileName(given: string): { file: string } | { error: string } {
  const name = given.replace(WHITE_SPACE_AT_THE_ENDS, "");
  if (name.includes("/") || name.includes("\\")) return { error: "a file name has no folder in it: `/` and `\\` are not allowed" };
  if (hasForbidden(name)) return { error: "a file name cannot have `:`, `*`, `?`, `\"`, `<`, `>`, `|` or a control character" };
  const file = name.endsWith(".toml") ? name : `${name}.toml`;
  // (Nothing before the extension, which is what no name at all comes to as well.)
  if (file === ".toml") return { error: "a file name is needed" };
  const stem = file.split(".")[0] ?? "";
  if (WINDOWS_DEVICE_NAMES.has(stem.replace(/[A-Z]/gu, (letter) => letter.toLowerCase()))) return { error: `${stem} is a name that Windows keeps for itself` };
  if (new TextEncoder().encode(file).length > MAX_FILE_NAME_BYTES) return { error: "a file name is at most 255 bytes long" };
  return { file };
}
