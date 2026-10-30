// The words that say what taking out a lane or a group comes to (the "×" of a lane's or a group's card asks with them, and so does the context menu of a node): what goes with it, and what stays.
import type { Removal } from "./selectors";

/** "1 effect", "2 effects". */
const counted = (n: number, word: string): string => `${String(n)} ${word}${n === 1 ? "" : "s"}`;

/** What taking out a lane or a group comes to, in words: what goes with it, and what stays: for a group its lanes, and for either the modulators that hear it, which hear every note then. */
export function removalWords(removal: Removal): string {
  const goes = removal.effects > 0 ? `its ${counted(removal.effects, "effect")} go${removal.effects === 1 ? "es" : ""} with it` : null;
  const stays = removal.lanes > 0 ? `its ${counted(removal.lanes, "lane")} ${removal.lanes === 1 ? "stays" : "stay"}, in no group` : null;
  const heard = removal.hearing > 0 ? `${counted(removal.hearing, "modulator")} that ${removal.hearing === 1 ? "hears it stays" : "hear it stay"}, hearing every note` : null;
  const parts = [goes, stays, heard].filter((part) => part !== null);
  return parts.length > 0 ? `: ${parts.join(", and ")}` : "";
}

/** What stays in a lane when its instrument is taken out of it, in words: its effects, or that it is left empty. */
export function keptWords(removal: Removal): string {
  return removal.effects > 0 ? `: its ${counted(removal.effects, "effect")} stay${removal.effects === 1 ? "s" : ""} in the lane` : ": the lane stays, empty";
}
