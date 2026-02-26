use std::path::PathBuf;
use std::sync::OnceLock;

use serde::Deserialize;

static CONFIG: OnceLock<Config> = OnceLock::new();

#[derive(Default, Deserialize)]
#[serde(default)]
pub struct Config {
    pub plugin_paths: PluginPaths,
}

#[derive(Default, Deserialize)]
#[serde(default)]
pub struct PluginPaths {
    pub clap: Vec<PathBuf>,
    pub vst3: Vec<PathBuf>,
    pub lv2: Vec<PathBuf>,
}

pub fn init(config: Config) {
    CONFIG.set(config).ok();
}

pub fn extra_clap_paths() -> &'static [PathBuf] {
    CONFIG
        .get()
        .map(|c| c.plugin_paths.clap.as_slice())
        .unwrap_or(&[])
}

#[allow(dead_code)]
pub fn extra_vst3_paths() -> &'static [PathBuf] {
    CONFIG
        .get()
        .map(|c| c.plugin_paths.vst3.as_slice())
        .unwrap_or(&[])
}

pub fn extra_lv2_paths() -> &'static [PathBuf] {
    CONFIG
        .get()
        .map(|c| c.plugin_paths.lv2.as_slice())
        .unwrap_or(&[])
}
