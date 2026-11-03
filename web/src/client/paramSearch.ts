// Finding a parameter of a plugin by name: a plugin can have thousands, and a person who wants one types some of
// what it is called (the TUI's `/` does the same).

/**
 * The parameters whose name has every word of `query` in it, whatever the case and wherever in the name, as the
 * plugin's own indices in the plugin's order. Nothing typed, or only blanks, is every parameter (the empty word that
 * splitting blanks leaves is in every name). The words are taken as text, never as a pattern.
 */
export function matchParams(params: readonly { index: number; name: string }[], query: string): number[] {
  const words = query.toLowerCase().split(/\s+/);
  return params.filter((param) => {
    const name = param.name.toLowerCase();
    return words.every((word) => name.includes(word));
  }).map((param) => param.index);
}
