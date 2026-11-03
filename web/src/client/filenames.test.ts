import { describe, expect, test } from "vitest";

import { filenamesFixture } from "./fixtures";
import { sessionFileName } from "./filenames";

// The rule for the name a session is saved as (`session::plain_file_name`): the mock engine's, pinned to the controller's by the names it has made of a list of
// names (the fixture): the file, or the words for what is wrong.
describe("the file a session is saved as", () => {
  test("is what the controller makes of each name of its fixture: the file it is saved as, or its words for what is wrong with the name", () => {
    expect(filenamesFixture.names.length).toBeGreaterThan(50);
    const wrong = filenamesFixture.names.flatMap((entry) => {
      const made = sessionFileName(entry.given);
      const expected = "file" in entry ? { file: entry.file } : { error: entry.error };
      return JSON.stringify(made) === JSON.stringify(expected) ? [] : [`${JSON.stringify(entry.given)}: ${JSON.stringify(made)}, and the controller says ${JSON.stringify(expected)}`];
    });
    expect(wrong).toEqual([]);
  });

  test("has been tried with names that are fine and with each kind of what is not: the fixture is not thin", () => {
    const refusals = new Set(filenamesFixture.names.flatMap((entry) => ("error" in entry ? [entry.error.replace(/^\S+ is a name/, "X is a name")] : [])));
    expect([...refusals].sort()).toEqual(
      [
        "a file name cannot have `:`, `*`, `?`, `\"`, `<`, `>`, `|` or a control character",
        "a file name has no folder in it: `/` and `\\` are not allowed",
        "a file name is at most 255 bytes long",
        "a file name is needed",
        "X is a name that Windows keeps for itself",
      ].sort(),
    );
    expect(filenamesFixture.names.filter((entry) => "file" in entry).length).toBeGreaterThanOrEqual(12);
  });

  test("counts bytes and not characters, and the extension with the name", () => {
    expect(sessionFileName("é".repeat(125))).toEqual({ file: `${"é".repeat(125)}.toml` });
    expect(sessionFileName("é".repeat(126))).toEqual({ error: "a file name is at most 255 bytes long" });
    expect(sessionFileName(`${"a".repeat(250)}.toml`)).toEqual({ file: `${"a".repeat(250)}.toml` });
  });
});
