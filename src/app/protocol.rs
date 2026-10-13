//! The wire protocol: the types that cross the socket between the controller and
//! a browser, and the TypeScript that is generated from them; see
//! `docs/web-ui.md` §7.
//!
//! The types themselves live next to what they describe (`NodeId` in `model`,
//! `Intent` and `Event` in `intent`, ...) and derive `serde` always and `ts_rs::TS`
//! under the `web` feature. This module lists the roots of the protocol and
//! writes their TypeScript, into `web/src/protocol/generated/`, which is committed.
//! A test fails when what is committed is not what the types generate now:
//!
//! ```text
//! TANG_UPDATE_TYPES=1 cargo test --features web typescript
//! ```
//!
//! rewrites the directory.

#[cfg(test)]
mod tests {
    #[test]
    fn node_ids_are_plain_numbers_on_the_wire() {
        use crate::app::model::NodeId;
        assert_eq!(serde_json::to_string(&NodeId(7)).unwrap(), "7");
        assert_eq!(serde_json::from_str::<NodeId>("7").unwrap(), NodeId(7));
    }
}

#[cfg(all(test, feature = "web"))]
mod typescript {
    use std::collections::BTreeMap;
    use std::fs;
    use std::path::{Path, PathBuf};

    use ts_rs::{Config, TS};

    use crate::app::model::NodeId;

    /// Where the generated TypeScript is committed.
    fn generated_dir() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("web/src/protocol/generated")
    }

    /// Write the TypeScript of every root type of the protocol, and of what each
    /// uses, into `dir`: one file per type.
    fn export_typescript(dir: &Path) -> Result<(), ts_rs::ExportError> {
        // Not `Config::from_env()`: what is generated must not depend on the
        // environment of whoever runs the test. `u64` counts (revisions) stay far
        // below 2^53, so they are plain numbers rather than `bigint`.
        let cfg = Config::new().with_large_int("number").with_out_dir(dir);
        NodeId::export_all(&cfg)?;
        Ok(())
    }

    /// The `.ts` files in a directory, by name.
    fn typescript_files(dir: &Path) -> BTreeMap<String, String> {
        let Ok(entries) = fs::read_dir(dir) else { return BTreeMap::new() };
        entries
            .map(|entry| entry.unwrap().path())
            .filter(|path| path.extension().is_some_and(|ext| ext == "ts"))
            .map(|path| (path.file_name().unwrap().to_string_lossy().into_owned(), fs::read_to_string(&path).unwrap()))
            .collect()
    }

    /// How the `.ts` files in `committed` differ from those in `fresh`, one line
    /// each, by file name. Other files in `committed` are not looked at.
    fn differences(committed: &Path, fresh: &Path) -> Vec<String> {
        let (committed, fresh) = (typescript_files(committed), typescript_files(fresh));
        let mut names: Vec<&String> = committed.keys().chain(fresh.keys()).collect();
        names.sort();
        names.dedup();
        names
            .into_iter()
            .filter_map(|name| match (committed.get(name), fresh.get(name)) {
                (Some(a), Some(b)) if a == b => None,
                (Some(_), Some(_)) => Some(format!("{name} is out of date")),
                (Some(_), None) => Some(format!("{name} is not generated any more")),
                (None, _) => Some(format!("{name} is not committed")),
            })
            .collect()
    }

    fn write(dir: &Path, name: &str, text: &str) {
        fs::write(dir.join(name), text).unwrap();
    }

    #[test]
    fn two_equal_directories_have_no_differences() {
        let (a, b) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
        for dir in [&a, &b] {
            write(dir.path(), "One.ts", "export type One = number;\n");
            write(dir.path(), "Two.ts", "export type Two = string;\n");
        }
        assert_eq!(differences(a.path(), b.path()), Vec::<String>::new());
    }

    #[test]
    fn a_changed_missing_and_extra_file_are_each_reported() {
        let (committed, fresh) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
        write(committed.path(), "Same.ts", "a");
        write(fresh.path(), "Same.ts", "a");
        write(committed.path(), "Changed.ts", "old");
        write(fresh.path(), "Changed.ts", "new");
        write(fresh.path(), "Missing.ts", "m");
        write(committed.path(), "Extra.ts", "e");
        assert_eq!(
            differences(committed.path(), fresh.path()),
            [
                "Changed.ts is out of date",
                "Extra.ts is not generated any more",
                "Missing.ts is not committed",
            ]
        );
    }

    #[test]
    fn a_file_that_is_not_typescript_is_left_alone() {
        let (committed, fresh) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
        write(committed.path(), "README.md", "notes");
        assert_eq!(differences(committed.path(), fresh.path()), Vec::<String>::new());
    }

    #[test]
    fn the_generated_typescript_is_up_to_date() {
        let committed = generated_dir();
        if std::env::var_os("TANG_UPDATE_TYPES").is_some() {
            fs::create_dir_all(&committed).unwrap();
            for entry in fs::read_dir(&committed).unwrap() {
                let path = entry.unwrap().path();
                if path.extension().is_some_and(|ext| ext == "ts") {
                    fs::remove_file(path).unwrap();
                }
            }
            export_typescript(&committed).unwrap();
            return;
        }
        let fresh = tempfile::tempdir().unwrap();
        export_typescript(fresh.path()).unwrap();
        let differences = differences(&committed, fresh.path());
        assert!(
            differences.is_empty(),
            "web/src/protocol/generated is stale:\n  {}\nrun `TANG_UPDATE_TYPES=1 cargo test --features web typescript` and commit the result",
            differences.join("\n  ")
        );
    }
}
