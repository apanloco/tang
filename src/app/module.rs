//! Modules: a chain of effects that is saved to a file and loaded into another session (step 104, the first of "design a set of modules, save them as a stand-alone module that you can
//! then load inside another instrument").
//!
//! A module is a **session file of one lane that has no instrument**: its effects in the order of its chain, each as a session file says an effect (the plugin, the preset it has, the
//! values that are not the plugin's own, its mix, the knobs pinned to its node, and whether the stage before it feeds it), and the audio cables that are between its effects and
//! nothing else (what goes into the chain and what comes out of it is the cables that a person draws once the module is in a session). The files are in one directory, the library
//! (`Controller::modules_dir`), one file for each module, and the name of a module is the name of its file without `.toml`. The catalog lists the modules with the plugins, as
//! `module:<name>` (`Catalog::set_modules`), so that what adds a plugin that stands by itself (`AddFreeEffect`) adds a module too: a lane of its own with the effects in it, bare, as
//! `AddFreeEffect` makes it.
//!
//! This file is the part that touches no controller: the names, the library's listing, the text of a module from the model, and what a text says. The controller's `save_module` and
//! `load_module` (in `apply.rs`) do the rest.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use crate::app::model::{NodeId, NodePath, SessionModel};
use crate::app::save;
use crate::plugin::PluginInfo;
use crate::plugin::chain::{CableFrom, CableTo};
use crate::session::{self, CablePlace, EffectConfig, Route, SaveCable, SaveInstrumentSlot, SaveSources};

/// What the catalog's id of a module begins with: `module:Space` is the module that the file `Space.toml` of the library is.
pub const PREFIX: &str = "module:";

/// The id that the catalog gives the module called `name`.
pub fn id_of(name: &str) -> String {
    format!("{PREFIX}{name}")
}

/// The name of the module that the catalog's `id` says, or none for the id of a plugin.
pub fn name_of(id: &str) -> Option<&str> {
    id.strip_prefix(PREFIX)
}

/// The name a module is saved under, as a file name that is a name and not a path (`session::plain_file_name`: no folder, none of the characters that a file cannot have, a name that Windows
/// does not keep for itself, at most 255 bytes) without the `.toml` that the file has. The refusal says what is wrong with the name.
pub fn clean_name(given: &str) -> Result<String, String> {
    let file = session::plain_file_name(given)?;
    // (`plain_file_name` adds the extension: a name that already has it is the same module.)
    let stem = file.strip_suffix(".toml").unwrap_or(&file);
    // (A name that ends in the extension itself is a file that the library would not list under it: the listing takes the name off the file, and the name that it makes has to be the one that was given.)
    if stem.ends_with(".toml") {
        return Err("the name of a module does not end in .toml: that is the extension of its file".to_string());
    }
    Ok(stem.to_string())
}

/// Where the module called `name` is in the library `dir`.
pub fn path_in(dir: &Path, name: &str) -> PathBuf {
    dir.join(format!("{name}.toml"))
}

/// A module as a file says it: the effects of its chain, as a session file says an effect, and the audio cables between them.
pub struct Module {
    pub effects: Vec<EffectConfig>,
    pub cables: Vec<ModuleCable>,
}

/// An audio cable of a module: from the output of one effect to the input of another, or to its second input, at a level.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ModuleCable {
    /// The place in the chain of the effect it starts at.
    pub from: usize,
    /// The place in the chain of the effect it ends at.
    pub to: usize,
    /// Whether it ends at the second input of that effect (`Plugin::side_input`).
    pub side: bool,
    pub gain: f32,
}

/// What a module's text says, or why it is not a module: a session file of one lane that has effects and no instrument, no group, no modulator and no source of notes, and no cable that
/// is not between two of its effects.
pub fn parse(text: &str) -> Result<Module, String> {
    let config = session::parse(text).map_err(|error| error.to_string())?;
    let [lane] = config.instruments.as_slice() else {
        return Err(format!("a module is one lane of effects, and this file has {} lanes", config.instruments.len()));
    };
    if lane.instrument.is_some() {
        return Err("a module is a chain of effects, and this lane has an instrument".to_string());
    }
    // (A lane's own modulators are moved to the session's own when a file is read, `session::parse`: a module that has any has `free_modulators`.)
    if !config.groups.is_empty() || !config.free_modulators.is_empty() || config.keys.is_some() || !config.devices.is_empty() {
        return Err("a module is a chain of effects and nothing else: it has no group, no modulator and no source of notes".to_string());
    }
    if lane.effects.is_empty() {
        return Err("a module has at least one effect".to_string());
    }
    let mut cables = Vec::new();
    for (i, cable) in config.cables.iter().enumerate() {
        let (from, to, side) = match (cable.from, cable.to) {
            (CablePlace::Effect { instrument: 0, effect: from }, CablePlace::Effect { instrument: 0, effect: to }) => (from, to, false),
            (CablePlace::Effect { instrument: 0, effect: from }, CablePlace::EffectSide { instrument: 0, effect: to }) => (from, to, true),
            _ => return Err(format!("cable[{i}] is not between two effects of the chain: a module has the cables that are inside it and no others")),
        };
        if from >= lane.effects.len() || to >= lane.effects.len() {
            return Err(format!("cable[{i}] names an effect that the chain has not"));
        }
        cables.push(ModuleCable { from, to, side, gain: cable.gain.unwrap_or(1.0) });
    }
    let effects = config.instruments.into_iter().next().map(|lane| lane.effects).unwrap_or_default();
    Ok(Module { effects, cables })
}

/// The text of a module made of the chain of the lane `node` is of (the node of a lane, or of an effect in its chain): the effects as they are now, in order, and the audio cables that
/// start and end at them. Refused, with the reason, for a node that is neither, a group's bus, and a lane that has no effect.
pub fn to_text(model: &SessionModel, node: NodeId) -> Result<String, String> {
    let inst = match model.locate(node) {
        Some(NodePath::Instrument(inst) | NodePath::Effect { inst, .. }) => inst,
        Some(NodePath::GroupEffect { .. } | NodePath::Group(_)) => return Err("a module is the chain of a lane: a group's bus is not saved".to_string()),
        _ => return Err("a module is made of the chain of a lane".to_string()),
    };
    let lane = &model.instruments[inst];
    if lane.effects.is_empty() {
        return Err("a lane that has no effect has no chain to save".to_string());
    }
    let place = |effect: NodeId| lane.effects.iter().position(|fx| fx.node == effect);
    // (The cables between two effects of the chain, in the order of their ids; a cable that has an end outside the chain is the session's and not the module's.)
    let cables: Vec<SaveCable> = model
        .cables
        .iter()
        .filter_map(|cable| {
            let CableFrom::Effect(from) = cable.from else { return None };
            let from = CablePlace::Effect { instrument: 0, effect: place(from)? };
            let to = match cable.to {
                CableTo::Effect(to) => CablePlace::Effect { instrument: 0, effect: place(to)? },
                CableTo::EffectSide(to) => CablePlace::EffectSide { instrument: 0, effect: place(to)? },
                CableTo::GroupBus(_) | CableTo::Master => return None,
            };
            Some(SaveCable { from, to, gain: cable.gain })
        })
        .collect();
    // (Where the effects are drawn is the page's of that session, and not the module's: a module is laid out where it is put.)
    let no_places = BTreeMap::new();
    let slot = SaveInstrumentSlot {
        range: None,
        transpose: 0,
        muted: false,
        solo: false,
        route: Route::None,
        instrument: None,
        effects: lane.effects.iter().map(|effect| save::effect_to_save(effect, &no_places)).collect(),
        pattern: None,
        group: None,
        ui: None,
    };
    let text = session::to_toml(&[slot], &[], &cables, &SaveSources::default(), None, None, None).map_err(|error| error.to_string())?;
    Ok(format!("# A chain of effects, saved by Tang as a module: add it to a session as an effect that stands by itself, and draw the cables that go in and out of it.\n{text}"))
}

/// The modules of the library `dir`, as the catalog lists them: each file `<name>.toml` that is a module, by its name, with how many effects it has where a plugin has its parameters. A
/// file that is not a module (or cannot be read) is left out, and said in the log; a directory that is not there has none.
pub fn list(dir: &Path) -> Vec<PluginInfo> {
    let Ok(entries) = std::fs::read_dir(dir) else { return Vec::new() };
    let mut found: Vec<PluginInfo> = entries
        .filter_map(Result::ok)
        .filter_map(|entry| {
            let path = entry.path();
            let name = path.file_name()?.to_str()?.strip_suffix(".toml")?.to_string();
            // (Only a name that this program could have saved under: the id goes to a client, and is loaded by it.)
            if clean_name(&name).ok().as_deref() != Some(name.as_str()) || !entry.file_type().is_ok_and(|kind| kind.is_file()) {
                return None;
            }
            match std::fs::read_to_string(&path).map_err(|error| error.to_string()).and_then(|text| parse(&text)) {
                Ok(module) => Some(PluginInfo { id: id_of(&name), name, is_instrument: false, param_count: module.effects.len(), preset_count: 0, path: path.to_string_lossy().into_owned(), scan_ms: 0 }),
                Err(error) => {
                    log::warn!("{} is not a module and is left out of the library: {error}", path.display());
                    None
                }
            }
        })
        .collect();
    found.sort_by_key(|module| module.name.to_lowercase());
    found
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_module_is_known_by_the_id_that_the_catalog_gives_it_and_a_plugin_is_not() {
        assert_eq!(id_of("Space"), "module:Space");
        assert_eq!(name_of("module:Space"), Some("Space"));
        assert_eq!(name_of("builtin:reverb"), None);
        assert_eq!(name_of("module:"), Some(""));
    }

    #[test]
    fn the_name_of_a_module_is_a_plain_file_name_without_its_extension() {
        assert_eq!(clean_name("Space"), Ok("Space".to_string()));
        assert_eq!(clean_name("  Space  "), Ok("Space".to_string()));
        assert_eq!(clean_name("Space.toml"), Ok("Space".to_string()));
        assert_eq!(clean_name("Big room 2"), Ok("Big room 2".to_string()));
        for bad in ["", "   ", ".toml", "a/b", "..\\b", "a:b", "what?", "con", "tab\tname"] {
            assert!(clean_name(bad).is_err(), "{bad:?}");
        }
        assert!(clean_name(&"x".repeat(300)).is_err());
        // The name of the file is the name and the extension: a name with the extension twice is a file that the library lists under another name, so it is not one.
        for twice in ["a.toml.toml", "Space.toml.toml"] {
            assert!(clean_name(twice).is_err_and(|why| why.contains("does not end in .toml")), "{twice:?}");
        }
        assert_eq!(clean_name("a.tomlx"), Ok("a.tomlx".to_string()), "only the extension is the extension");
        assert_eq!(clean_name("x.toml "), Ok("x".to_string()), "the spaces round a name go first");
    }

    #[test]
    fn a_module_that_is_a_chain_of_effects_is_read_as_one_and_what_is_not_one_is_refused_with_the_reason() {
        let chain = "[[instrument]]\n[[instrument.effect]]\nplugin = \"builtin:filter\"\n[[instrument.effect]]\nplugin = \"builtin:reverb\"\nmix = 0.5\n[[cable]]\nfrom = \"instrument[0].effect[0]\"\nto = \"instrument[0].effect[1]\"\ngain = 0.5\n";
        let module = parse(chain).unwrap();
        assert_eq!(module.effects.iter().map(|effect| effect.plugin.as_str()).collect::<Vec<_>>(), ["builtin:filter", "builtin:reverb"]);
        assert_eq!(module.cables, [ModuleCable { from: 0, to: 1, side: false, gain: 0.5 }]);
        let side = format!("{chain}[[cable]]\nfrom = \"instrument[0].effect[1]\"\nto = \"instrument[0].effect[0].side\"\n");
        assert_eq!(parse(&side).unwrap().cables[1], ModuleCable { from: 1, to: 0, side: true, gain: 1.0 });

        let refused = |text: &str| parse(text).err().unwrap_or_else(|| "was read".to_string());
        assert!(refused("").contains("0 lanes"), "{}", refused(""));
        assert!(refused("[[instrument]]\nplugin = \"builtin:osc\"\n[[instrument.effect]]\nplugin = \"builtin:filter\"\n").contains("has an instrument"));
        assert!(refused("[[instrument]]\n").contains("at least one effect"));
        assert!(refused("[[instrument]]\n[[instrument]]\n").contains("2 lanes"));
        assert!(refused("[[group]]\n[[instrument]]\n[[instrument.effect]]\nplugin = \"builtin:filter\"\n").contains("nothing else"));
        assert!(refused("[[modulator]]\n[[instrument]]\n[[instrument.effect]]\nplugin = \"builtin:filter\"\n").contains("nothing else"));
        // (A source of notes, a device and a lane's own modulator, which is moved to the session's own when a file is read and is a modulator all the same: none is the chain's.)
        let one = "[[instrument]]\n[[instrument.effect]]\nplugin = \"builtin:filter\"\n";
        assert!(refused(&format!("[keys]\n{one}")).contains("nothing else"), "{}", refused(&format!("[keys]\n{one}")));
        assert!(refused(&format!("[[device]]\nname = \"Launchkey\"\n{one}")).contains("nothing else"));
        assert!(refused(&format!("{one}[[instrument.modulator]]\ntype = \"lfo\"\n")).contains("nothing else"));
        let to_master = "[[instrument]]\n[[instrument.effect]]\nplugin = \"builtin:filter\"\n[[cable]]\nfrom = \"instrument[0].effect[0]\"\nto = \"master\"\n";
        assert!(refused(to_master).contains("cable[0] is not between two effects"), "{}", refused(to_master));
        let beyond = "[[instrument]]\n[[instrument.effect]]\nplugin = \"builtin:filter\"\n[[cable]]\nfrom = \"instrument[0].effect[0]\"\nto = \"instrument[0].effect[3]\"\n";
        assert!(refused(beyond).contains("an effect that the chain has not"), "{}", refused(beyond));
        // The end of a chain of one effect is effect[0]: effect[1] is the first that is not there, at either end of a cable.
        let between = |from: &str, to: &str| format!("[[instrument]]\n[[instrument.effect]]\nplugin = \"builtin:filter\"\n[[cable]]\nfrom = \"{from}\"\nto = \"{to}\"\n");
        assert!(refused(&between("instrument[0].effect[0]", "instrument[0].effect[1]")).contains("an effect that the chain has not"));
        assert!(refused(&between("instrument[0].effect[1]", "instrument[0].effect[0]")).contains("an effect that the chain has not"));
        assert!(refused(&between("instrument[0].effect[0]", "instrument[0].effect[1].side")).contains("an effect that the chain has not"));
        // A cable that names the effects of another lane is not between two effects of this chain, whichever end it is at and whichever input it ends at.
        for (from, to) in [
            ("instrument[1].effect[0]", "instrument[0].effect[0]"),
            ("instrument[0].effect[0]", "instrument[1].effect[0]"),
            ("instrument[1].effect[0]", "instrument[0].effect[0].side"),
            ("instrument[0].effect[0]", "instrument[1].effect[0].side"),
        ] {
            assert!(refused(&between(from, to)).contains("cable[0] is not between two effects"), "{from} to {to}: {}", refused(&between(from, to)));
        }
        assert!(refused("this is not toml [").contains("TOML") || !refused("this is not toml [").is_empty());
    }

    #[test]
    fn the_library_lists_the_files_that_are_modules_by_name_and_leaves_the_others_out() {
        let dir = tempfile::tempdir().unwrap();
        let chain = |plugins: &[&str]| plugins.iter().fold("[[instrument]]\n".to_string(), |text, plugin| format!("{text}[[instrument.effect]]\nplugin = \"{plugin}\"\n"));
        std::fs::write(dir.path().join("Space.toml"), chain(&["builtin:filter", "builtin:reverb"])).unwrap();
        std::fs::write(dir.path().join("alpha.toml"), chain(&["builtin:delay"])).unwrap();
        std::fs::write(dir.path().join("broken.toml"), "[[instrument]]\n").unwrap();
        std::fs::write(dir.path().join("notes.txt"), "not a module").unwrap();
        // (A file that is valid and has a name that no module is saved under: the listing would call it `twice.toml`, which is not a name that this program makes of `twice.toml`.)
        std::fs::write(dir.path().join("twice.toml.toml"), chain(&["builtin:delay"])).unwrap();
        std::fs::create_dir(dir.path().join("folder.toml")).unwrap();
        let listed = list(dir.path());
        assert_eq!(
            listed.iter().map(|module| (module.id.as_str(), module.name.as_str(), module.param_count, module.preset_count, module.is_instrument)).collect::<Vec<_>>(),
            [("module:alpha", "alpha", 1, 0, false), ("module:Space", "Space", 2, 0, false)]
        );
        assert!(list(&dir.path().join("not-there")).is_empty());
    }
}
