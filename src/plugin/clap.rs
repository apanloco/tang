use std::ffi::CStr;
use std::path::Path;
use std::rc::Rc;
use std::sync::Arc;

use clack_extensions::gui::{GuiSize, HostGui, HostGuiImpl, PluginGui};
use clack_extensions::latency::PluginLatency;
use clack_extensions::log::{HostLog, HostLogImpl, LogSeverity};
use clack_extensions::audio_ports::{
    AudioPortInfoBuffer, HostAudioPorts, HostAudioPortsImpl, PluginAudioPorts, RescanType,
};
use clack_extensions::params::{
    HostParams, HostParamsImplMainThread, HostParamsImplShared, ParamClearFlags, ParamInfoBuffer,
    ParamRescanFlags, PluginParams,
};
use clack_extensions::preset_discovery::HostPresetLoadImpl;
use clack_extensions::preset_discovery::prelude::{
    FileType, Flags, HostPresetLoad, IndexerImpl, Location, LocationInfo, MetadataReceiverImpl,
    PluginPresetLoad, PresetDiscoveryFactory, Provider, Soundpack, Timestamp, UniversalPluginId,
};
use clack_host::events::event_types::{ParamValueEvent, TransportEvent, TransportFlags};
use clack_host::events::{EventFlags, EventHeader};
use clack_host::prelude::*;
use clack_host::process::StartedPluginAudioProcessor;
use clack_host::utils::{BeatTime, Cookie, SecondsTime};

use super::clap_callbacks;
use super::clap_editor::{self, Asked};
use super::editors;
use super::{ParameterInfo, Plugin, PluginInfo, Preset};

// ---------------------------------------------------------------------------
// Host handler types (minimal, no-op callbacks)
// ---------------------------------------------------------------------------

struct TangHost;
/// What the host's threads share for one plugin: what the plugin asks of the host about its window (`clap_editor::Asked`).
#[derive(Default)]
struct TangHostShared {
    asked: Arc<Asked>,
}
#[derive(Default)]
struct TangHostMainThread {
    /// What the plugin asks of the host's run loop on Linux (`timer-support`, `posix-fd-support`): served by the loop that turns the windows.
    #[cfg(target_os = "linux")]
    run_loop: super::clap_run_loop::PluginRunLoop,
}

impl HostHandlers for TangHost {
    type Shared<'a> = TangHostShared;
    type MainThread<'a> = TangHostMainThread;
    type AudioProcessor<'a> = ();

    fn declare_extensions(builder: &mut HostExtensions<Self>, _shared: &Self::Shared<'_>) {
        builder.register::<HostAudioPorts>();
        builder.register::<HostGui>();
        builder.register::<HostLog>();
        builder.register::<HostParams>();
        builder.register::<HostPresetLoad>();
        #[cfg(target_os = "linux")]
        {
            builder.register::<clack_extensions::timer::HostTimer>();
            builder.register::<clack_extensions::posix_fd::HostPosixFd>();
        }
    }
}

impl<'a> SharedHandler<'a> for TangHostShared {
    fn request_restart(&self) {
        log::debug!("CLAP plugin requested restart (ignored)");
    }
    fn request_process(&self) {
        log::debug!("CLAP plugin requested process (ignored)");
    }
    fn request_callback(&self) {
        // (From any thread: the call is the main thread's, made by whoever turns the controller, `plugin::clap_callbacks::run`.)
        self.asked.request_callback();
    }
}

impl<'a> MainThreadHandler<'a> for TangHostMainThread {
    #[cfg(target_os = "linux")]
    fn initialized(&mut self, instance: InitializedPluginHandle<'a>) {
        self.run_loop.initialized(instance);
    }
}

#[cfg(target_os = "linux")]
impl clack_extensions::timer::HostTimerImpl for TangHostMainThread {
    fn register_timer(&mut self, period_ms: u32) -> Result<clack_extensions::timer::TimerId, HostError> {
        self.run_loop.register_timer(period_ms)
    }

    fn unregister_timer(&mut self, timer_id: clack_extensions::timer::TimerId) -> Result<(), HostError> {
        self.run_loop.unregister_timer(timer_id)
    }
}

#[cfg(target_os = "linux")]
impl clack_extensions::posix_fd::HostPosixFdImpl for TangHostMainThread {
    fn register_fd(&mut self, fd: std::os::fd::RawFd, flags: clack_extensions::posix_fd::FdFlags) -> Result<(), HostError> {
        self.run_loop.register_fd(fd, flags)
    }

    fn modify_fd(&mut self, fd: std::os::fd::RawFd, flags: clack_extensions::posix_fd::FdFlags) -> Result<(), HostError> {
        self.run_loop.modify_fd(fd, flags)
    }

    fn unregister_fd(&mut self, fd: std::os::fd::RawFd) -> Result<(), HostError> {
        self.run_loop.unregister_fd(fd)
    }
}

impl HostLogImpl for TangHostShared {
    fn log(&self, severity: LogSeverity, message: &str) {
        let level = match severity {
            LogSeverity::Debug => log::Level::Debug,
            LogSeverity::Info => log::Level::Info,
            LogSeverity::Warning => log::Level::Warn,
            _ => log::Level::Error,
        };
        log::log!(target: "tang::plugin::clap::said", level, "{message}");
    }
}

/// What a plugin says to the host about its window (step 114, part C): written down for the window's loop to answer, from whichever thread the plugin says it on.
impl HostGuiImpl for TangHostShared {
    fn resize_hints_changed(&self) {
        log::debug!("CLAP gui: resize_hints_changed (ignored)");
    }

    fn request_resize(&self, new_size: GuiSize) -> Result<(), HostError> {
        self.asked.request_resize(new_size.width, new_size.height);
        Ok(())
    }

    fn request_show(&self) -> Result<(), HostError> {
        // (A window is opened when a person asks for it, from the page, the terminal or `tang window`: not by a plugin.)
        Err(HostError::Message("Tang opens a plugin's window when a person asks for it"))
    }

    fn request_hide(&self) -> Result<(), HostError> {
        // (The button of a plugin's own that hides its window: the window is closed, which hides it, and is made again when a person asks for it.)
        self.asked.closed();
        Ok(())
    }

    fn closed(&self, was_destroyed: bool) {
        log::debug!("CLAP gui: the plugin closed its window (destroyed: {was_destroyed})");
        self.asked.closed();
    }
}

impl HostParamsImplShared for TangHostShared {
    fn request_flush(&self) {
        log::debug!("CLAP params: request_flush (ignored)");
    }
}

impl HostParamsImplMainThread for TangHostMainThread {
    fn rescan(&mut self, _flags: ParamRescanFlags) {
        log::debug!("CLAP params: rescan (ignored)");
    }
    fn clear(&mut self, _param_id: ClapId, _flags: ParamClearFlags) {
        log::debug!("CLAP params: clear (ignored)");
    }
}

impl HostPresetLoadImpl for TangHostMainThread {
    fn on_error(
        &mut self,
        _location: Location,
        _load_key: Option<&CStr>,
        os_error: i32,
        message: Option<&CStr>,
    ) {
        log::warn!("CLAP preset load error: os_error={os_error}, message={message:?}");
    }
    fn loaded(&mut self, _location: Location, _load_key: Option<&CStr>) {
        log::info!("CLAP preset loaded successfully");
    }
}

impl HostAudioPortsImpl for TangHostMainThread {
    fn is_rescan_flag_supported(&self, _flag: RescanType) -> bool {
        false
    }
    fn rescan(&mut self, _flag: RescanType) {
        log::debug!("CLAP audio_ports: rescan (ignored)");
    }
}

// ---------------------------------------------------------------------------
// Preset discovery helpers
// ---------------------------------------------------------------------------

struct TangIndexer {
    locations: Vec<TangLocation>,
    file_extensions: Vec<String>,
}

#[derive(Clone)]
struct TangLocation {
    is_plugin: bool,
    file_path: Option<String>,
}

impl IndexerImpl for TangIndexer {
    fn declare_filetype(&mut self, file_type: FileType) -> Result<(), HostError> {
        if let Some(ext) = file_type.file_extension.and_then(|s| s.to_str().ok()) {
            log::debug!("CLAP preset discovery: declared filetype extension={ext}");
            self.file_extensions.push(ext.to_string());
        }
        Ok(())
    }
    fn declare_location(&mut self, location: LocationInfo) -> Result<(), HostError> {
        match location.location {
            Location::Plugin => {
                self.locations.push(TangLocation {
                    is_plugin: true,
                    file_path: None,
                });
            }
            Location::File { path } => {
                self.locations.push(TangLocation {
                    is_plugin: false,
                    file_path: path.to_str().ok().map(String::from),
                });
            }
        }
        Ok(())
    }
    fn declare_soundpack(&mut self, _soundpack: Soundpack) -> Result<(), HostError> {
        Ok(())
    }
}

struct TangMetadataReceiver {
    presets: Vec<(String, Option<String>)>,
}

impl MetadataReceiverImpl for TangMetadataReceiver {
    fn on_error(&mut self, error_code: i32, error_message: Option<&CStr>) {
        log::debug!("CLAP preset discovery error {error_code}: {error_message:?}");
    }

    fn begin_preset(
        &mut self,
        name: Option<&CStr>,
        load_key: Option<&CStr>,
    ) -> Result<(), HostError> {
        let name_str = name
            .and_then(|s| s.to_str().ok())
            .unwrap_or("Unknown")
            .to_string();
        let load_key_str = load_key.and_then(|s| s.to_str().ok()).map(String::from);
        self.presets.push((name_str, load_key_str));
        Ok(())
    }

    fn add_plugin_id(&mut self, _plugin_id: UniversalPluginId) {}
    fn set_soundpack_id(&mut self, _soundpack_id: &CStr) {}
    fn set_flags(&mut self, _flags: Flags) {}
    fn add_creator(&mut self, _creator: &CStr) {}
    fn set_description(&mut self, _description: &CStr) {}
    fn set_timestamps(
        &mut self,
        _creation_time: Option<Timestamp>,
        _modification_time: Option<Timestamp>,
    ) {
    }
    fn add_feature(&mut self, _feature: &CStr) {}
    fn add_extra_info(&mut self, _key: &CStr, _value: &CStr) {}
}

struct ClapPresetData {
    is_plugin_location: bool,
    file_path: Option<String>,
    load_key: Option<String>,
}

/// Recursively collect files matching the given extensions from a directory.
fn collect_preset_files(dir: &str, extensions: &[String]) -> Vec<String> {
    let mut files = Vec::new();
    let path = Path::new(dir);
    if !path.is_dir() {
        // Single file, not a directory
        files.push(dir.to_string());
        return files;
    }
    let mut stack = vec![path.to_path_buf()];
    while let Some(current) = stack.pop() {
        let entries = match std::fs::read_dir(&current) {
            Ok(e) => e,
            Err(_) => continue,
        };
        for entry in entries.flatten() {
            let entry_path = entry.path();
            if entry_path.is_dir() {
                stack.push(entry_path);
            } else if extensions.is_empty()
                || entry_path
                    .extension()
                    .and_then(|e| e.to_str())
                    .is_some_and(|ext| extensions.iter().any(|fe| fe == ext))
            {
                if let Some(s) = entry_path.to_str() {
                    files.push(s.to_string());
                }
            }
        }
    }
    files.sort();
    files
}

fn discover_presets(bundle: &PluginBundle, host_info: &HostInfo) -> Vec<(Preset, ClapPresetData)> {
    let factory: PresetDiscoveryFactory = match bundle.get_factory() {
        Some(f) => f,
        None => {
            log::debug!("CLAP bundle has no preset discovery factory");
            return Vec::new();
        }
    };
    log::info!(
        "CLAP preset discovery: {} providers",
        factory.provider_count()
    );

    let mut result = Vec::new();

    for descriptor in factory.provider_descriptors() {
        let provider_id = match descriptor.id() {
            Some(id) => id,
            None => continue,
        };

        let mut provider = match Provider::instantiate(
            TangIndexer {
                locations: Vec::new(),
                file_extensions: Vec::new(),
            },
            bundle,
            provider_id,
            host_info,
        ) {
            Ok(p) => p,
            Err(e) => {
                log::warn!("Failed to instantiate preset provider: {e}");
                continue;
            }
        };

        let locations: Vec<TangLocation> = provider.indexer().locations.clone();
        let extensions: Vec<String> = provider.indexer().file_extensions.clone();
        log::info!(
            "CLAP preset discovery: provider declared {} locations, {} filetypes",
            locations.len(),
            extensions.len()
        );

        for location in &locations {
            log::debug!(
                "CLAP preset discovery: scanning location is_plugin={} path={:?}",
                location.is_plugin,
                location.file_path
            );

            if location.is_plugin {
                let mut receiver = TangMetadataReceiver {
                    presets: Vec::new(),
                };
                provider.get_metadata(Location::Plugin, &mut receiver);
                for (name, load_key) in receiver.presets {
                    let idx = result.len();
                    result.push((
                        Preset {
                            name,
                            id: idx.to_string(),
                        },
                        ClapPresetData {
                            is_plugin_location: true,
                            file_path: None,
                            load_key,
                        },
                    ));
                }
            } else if let Some(ref dir_path) = location.file_path {
                // Walk the directory for matching files
                let files = collect_preset_files(dir_path, &extensions);
                for file_path in &files {
                    let c_path = match std::ffi::CString::new(file_path.as_str()) {
                        Ok(p) => p,
                        Err(_) => continue,
                    };
                    let mut receiver = TangMetadataReceiver {
                        presets: Vec::new(),
                    };
                    provider.get_metadata(Location::File { path: &c_path }, &mut receiver);
                    for (name, load_key) in receiver.presets {
                        let idx = result.len();
                        result.push((
                            Preset {
                                name,
                                id: idx.to_string(),
                            },
                            ClapPresetData {
                                is_plugin_location: false,
                                file_path: Some(file_path.clone()),
                                load_key,
                            },
                        ));
                    }
                }
            }
        }
    }

    result
}

// ---------------------------------------------------------------------------
// ClapPlugin
// ---------------------------------------------------------------------------

pub struct ClapPlugin {
    name: String,
    is_instrument: bool,
    sample_rate: f32,
    audio_in_channel_count: usize,
    audio_out_channel_count: usize,
    /// How many frames late what the plugin makes is, as it said when it was activated (`Plugin::latency`). A plugin that changes it while it runs, and tells the host by
    /// `clap_host_latency.changed`, is not followed: the plugin is on the audio thread by then, and the extension is the main thread's.
    latency: u32,
    /// The tempo that the session goes by, in beats a minute, as the graph told it before the block (`Plugin::set_tempo`, step 120): the plugin is given it as the transport of the block (`transport_event`). None until it is told, which
    /// is a plugin that is given no transport at all, as every CLAP plugin was before.
    tempo: Option<f64>,
    params_ext: Option<PluginParams>,
    params_cache: Vec<ParameterInfo>,
    param_ids: Vec<ClapId>,
    pending_param_changes: Vec<(ClapId, f64)>,
    preset_cache: Vec<Preset>,
    preset_data: Vec<ClapPresetData>,
    preset_load_ext: Option<PluginPresetLoad>,
    /// The plugin's own window in the registry of the windows (step 114, part C; `plugin::editors`): made when the plugin is loaded if it has a `gui` extension, and forgotten, with its window closed, first thing when the
    /// plugin is let go of.
    editor_id: Option<editors::EditorId>,
    /// Where the plugin is in the table of the calls that it asks the main thread for (step 115; `plugin::clap_callbacks`): taken out first thing when the plugin is let go of.
    callback_id: clap_callbacks::CallbackId,
    /// What the registries look at to know that the plugin is still there (`editors::register`, `clap_callbacks::register`).
    _alive: Arc<()>,
    _bundle: PluginBundle,
    instance: PluginInstance<TangHost>,
    audio_processor: Option<StartedPluginAudioProcessor<TangHost>>,
    // Pre-allocated buffers
    output_ports: AudioPorts,
    output_port_channel_counts: Vec<u32>,
    output_channel_bufs: Vec<Vec<f32>>,
    input_ports: AudioPorts,
    input_port_channel_counts: Vec<u32>,
    input_channel_bufs: Vec<Vec<f32>>,
    event_buffer: EventBuffer,
}

// Safety: PluginInstance is !Send because CLAP enforces main-thread affinity for
// certain operations (e.g. init, activate, deactivate, destroy). We satisfy this:
// the plugin is created, activated, and preset-loaded on the main thread, then
// moved (by value) into the PluginChain which is moved into the audio callback
// closure — single owner, no concurrent access. On the audio thread only
// process() is called, via the StartedPluginAudioProcessor handle. On shutdown
// the plugin is sent back to the main thread for deactivation and drop.
unsafe impl Send for ClapPlugin {}

impl Drop for ClapPlugin {
    fn drop(&mut self) {
        // The calls it asked for first, and its window: neither is for a plugin that is destroyed.
        clap_callbacks::forget(self.callback_id);
        if let Some(editor) = self.editor_id {
            editors::forget(editor);
        }
        if let Some(processor) = self.audio_processor.take() {
            let stopped = processor.stop_processing();
            self.instance.deactivate(stopped);
        }
    }
}

// ---------------------------------------------------------------------------
// Enumeration (unchanged)
// ---------------------------------------------------------------------------

/// Enumerate all CLAP plugins found on the system.
pub fn enumerate_plugins() -> Vec<PluginInfo> {
    let mut plugins = Vec::new();

    let bundle_paths: Vec<_> = clack_finder::ClapFinder::from_standard_paths()
        .into_iter()
        .chain(extra_clap_bundles())
        .collect();

    for bundle_path in bundle_paths {
        let start = std::time::Instant::now();
        match scan_bundle(&bundle_path) {
            Some(found) => {
                let elapsed_ms = start.elapsed().as_millis() as u64;
                let n = found.len().max(1) as u64;
                let per_plugin = elapsed_ms / n;
                for mut p in found {
                    p.scan_ms = per_plugin;
                    plugins.push(p);
                }
            }
            None => {
                log::warn!("Failed to scan CLAP bundle: {}", bundle_path.display());
            }
        }
    }

    plugins
}

/// Resolve extra CLAP paths from config. Paths pointing to `.clap` files are
/// returned directly (bypassing ClapFinder, which rejects flat files on macOS).
/// Paths pointing to directories are scanned via ClapFinder.
fn extra_clap_bundles() -> Vec<std::path::PathBuf> {
    let extra = crate::config::extra_clap_paths();
    if extra.is_empty() {
        return Vec::new();
    }

    let mut files = Vec::new();
    let mut dirs = Vec::new();
    for p in extra {
        if p.is_file() {
            files.push(p.clone());
        } else if p.is_dir() {
            dirs.push(p.clone());
        }
    }

    if !dirs.is_empty() {
        for bundle_path in clack_finder::ClapFinder::new(dirs) {
            files.push(bundle_path);
        }
    }

    files
}

fn scan_bundle(path: &Path) -> Option<Vec<PluginInfo>> {
    use clack_host::plugin::features::INSTRUMENT;

    // Safety: loading external dynamic libraries is inherently unsafe
    let bundle = unsafe { PluginBundle::load(path) }.ok()?;
    let factory = bundle.get_plugin_factory()?;

    let host_info =
        HostInfo::new("tang", "akerud", "https://github.com/akerud/tang", "0.1.0").ok()?;

    let mut found = Vec::new();
    for descriptor in factory.plugin_descriptors() {
        let id = descriptor.id()?.to_str().ok()?.to_string();
        let name = descriptor
            .name()
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_else(|| id.clone());
        let is_instrument = descriptor.features().any(|f| f == INSTRUMENT);

        // Briefly instantiate to query param count
        let plugin_id = std::ffi::CString::new(id.as_str()).ok()?;
        let param_count = PluginInstance::<TangHost>::new(
            |_| TangHostShared::default(),
            |_| TangHostMainThread::default(),
            &bundle,
            &plugin_id,
            &host_info,
        )
        .ok()
        .and_then(|mut inst| {
            let ext: PluginParams = inst.plugin_shared_handle().get_extension()?;
            let mut handle = inst.plugin_handle();
            Some(ext.count(&mut handle) as usize)
        })
        .unwrap_or(0);

        let preset_count = discover_presets(&bundle, &host_info).len();

        found.push(PluginInfo {
            name,
            id,
            is_instrument,
            param_count,
            preset_count,
            path: path.to_string_lossy().to_string(),
            scan_ms: 0,
        });
    }

    Some(found)
}

// ---------------------------------------------------------------------------
// Loading
// ---------------------------------------------------------------------------

pub fn load(
    source: &str,
    sample_rate: f32,
    max_block_size: usize,
) -> anyhow::Result<Box<dyn Plugin>> {
    // Resolve plugin ID and bundle
    let (bundle, plugin_id_string, name, is_instrument) = find_plugin(source)?;
    load_found(bundle, plugin_id_string, name, is_instrument, sample_rate, max_block_size)
}

/// Make the plugin that a bundle has, once it is found (`load` finds it by its path or its id; a test hands over a bundle that is in the program, which has no file).
fn load_found(
    bundle: PluginBundle,
    plugin_id_string: String,
    name: String,
    is_instrument: bool,
    sample_rate: f32,
    max_block_size: usize,
) -> anyhow::Result<Box<dyn Plugin>> {
    let host_info = HostInfo::new("tang", "akerud", "https://github.com/akerud/tang", "0.1.0")?;

    let plugin_id =
        std::ffi::CString::new(plugin_id_string.as_str()).expect("plugin ID contains NUL");

    // Instantiate
    let mut instance = PluginInstance::<TangHost>::new(
        |_| TangHostShared::default(),
        |_shared| TangHostMainThread::default(),
        &bundle,
        &plugin_id,
        &host_info,
    )
    .map_err(|e| anyhow::anyhow!("Failed to instantiate CLAP plugin: {e}"))?;

    // Query audio output ports
    let (audio_out_channel_count, output_port_channel_counts) = {
        let audio_ports_ext: Option<PluginAudioPorts> =
            instance.plugin_shared_handle().get_extension();
        match audio_ports_ext {
            Some(ext) => {
                let mut handle = instance.plugin_handle();
                let mut buf = AudioPortInfoBuffer::new();
                let count = ext.count(&mut handle, false);
                let mut total_channels = 0u32;
                let mut port_channels = Vec::new();
                for i in 0..count {
                    if let Some(info) = ext.get(&mut handle, i, false, &mut buf) {
                        log::info!(
                            "CLAP audio output port {i}: channels={}, name={}",
                            info.channel_count,
                            String::from_utf8_lossy(info.name),
                        );
                        total_channels += info.channel_count;
                        port_channels.push(info.channel_count);
                    }
                }
                if total_channels == 0 {
                    // Fallback: assume stereo
                    log::warn!("CLAP plugin reports 0 output channels, assuming stereo");
                    (2usize, vec![2])
                } else {
                    (total_channels as usize, port_channels)
                }
            }
            None => {
                // No audio-ports extension — assume stereo
                log::warn!("CLAP plugin does not support audio-ports extension, assuming stereo");
                (2usize, vec![2])
            }
        }
    };

    // Query audio input ports
    let (audio_in_channel_count, input_port_channel_counts) = {
        let audio_ports_ext: Option<PluginAudioPorts> =
            instance.plugin_shared_handle().get_extension();
        match audio_ports_ext {
            Some(ext) => {
                let mut handle = instance.plugin_handle();
                let mut buf = AudioPortInfoBuffer::new();
                let count = ext.count(&mut handle, true);
                let mut total_channels = 0u32;
                let mut port_channels = Vec::new();
                for i in 0..count {
                    if let Some(info) = ext.get(&mut handle, i, true, &mut buf) {
                        log::info!(
                            "CLAP audio input port {i}: channels={}, name={}",
                            info.channel_count,
                            String::from_utf8_lossy(info.name),
                        );
                        total_channels += info.channel_count;
                        port_channels.push(info.channel_count);
                    }
                }
                (total_channels as usize, port_channels)
            }
            None => (0usize, Vec::new()),
        }
    };

    // Query parameters
    let params_ext: Option<PluginParams> = instance.plugin_shared_handle().get_extension();
    let (params_cache, param_ids) = match params_ext {
        Some(ext) => {
            let mut handle = instance.plugin_handle();
            let mut info_buf = ParamInfoBuffer::new();
            let count = ext.count(&mut handle);
            let mut params = Vec::with_capacity(count as usize);
            let mut ids = Vec::with_capacity(count as usize);
            for i in 0..count {
                if let Some(info) = ext.get_info(&mut handle, i, &mut info_buf) {
                    ids.push(info.id);
                    params.push(ParameterInfo {
                        index: i,
                        name: String::from_utf8_lossy(info.name).to_string(),
                        min: info.min_value as f32,
                        max: info.max_value as f32,
                        default: info.default_value as f32,
                        ..Default::default()
                    });
                }
            }
            log::info!("CLAP plugin has {} parameters", params.len());
            (params, ids)
        }
        None => {
            log::info!("CLAP plugin does not support params extension");
            (Vec::new(), Vec::new())
        }
    };

    // Discover presets
    let (preset_cache, preset_data): (Vec<Preset>, Vec<ClapPresetData>) =
        discover_presets(&bundle, &host_info).into_iter().unzip();

    // Query preset load extension
    let preset_load_ext: Option<PluginPresetLoad> = instance.plugin_shared_handle().get_extension();

    log::info!(
        "Loaded CLAP plugin: {name} (instrument={is_instrument}, output_channels={audio_out_channel_count}, params={}, presets={})",
        params_cache.len(),
        preset_cache.len(),
    );

    // Activate + start processing
    let config = PluginAudioConfiguration {
        sample_rate: sample_rate as f64,
        min_frames_count: 1,
        max_frames_count: max_block_size as u32,
    };

    let stopped = instance
        .activate(|_, _| (), config)
        .map_err(|e| anyhow::anyhow!("Failed to activate CLAP plugin: {e}"))?;

    // The latency, which a plugin has once it is active.
    let latency_ext: Option<PluginLatency> = instance.plugin_shared_handle().get_extension();
    let latency = latency_ext.map_or(0, |ext| ext.get(&mut instance.plugin_handle()));
    if latency > 0 {
        log::info!("CLAP plugin {name} has a latency of {latency} frames");
    }

    let started = stopped
        .start_processing()
        .map_err(|e| anyhow::anyhow!("Failed to start CLAP processing: {e}"))?;

    // Pre-allocate buffers
    let output_port_count = output_port_channel_counts.len();
    let output_ports = AudioPorts::with_capacity(audio_out_channel_count, output_port_count);
    let output_channel_bufs: Vec<Vec<f32>> =
        (0..audio_out_channel_count).map(|_| Vec::new()).collect();

    let input_port_count = input_port_channel_counts.len();
    let input_ports = AudioPorts::with_capacity(audio_in_channel_count, input_port_count);
    let input_channel_bufs: Vec<Vec<f32>> =
        (0..audio_in_channel_count).map(|_| Vec::new()).collect();

    let event_buffer = EventBuffer::new();

    // The plugin's own window, if it has one (its `gui` extension): in the registry of the windows for as long as the plugin is (`plugin::clap_editor`).
    let alive = Arc::new(());
    let gui: Option<PluginGui> = instance.plugin_shared_handle().get_extension();
    // What the plugin says to the host (of its window, and that it wants its main thread called) is written down by the host's handler that it was made with, and the editor and the calls answer that same one.
    let asked = instance.access_shared_handler(|shared| shared.asked.clone());
    // SAFETY: the entry is taken out of the table first thing when the plugin is let go of (`Drop for ClapPlugin`), and a plugin whose token is gone is not called.
    let callback_id = clap_callbacks::register(Rc::new(unsafe { clap_callbacks::RealCall::new(instance.plugin_handle()) }), asked.clone(), &alive);
    let editor_id = gui.map(|gui| {
        // SAFETY: the editor is forgotten, with its window closed, before the plugin is let go of (`Drop for ClapPlugin`), and calls nothing in a plugin whose token is gone.
        let calls = unsafe { clap_editor::RealGui::new(instance.plugin_handle(), gui) };
        editors::register(Rc::new(clap_editor::ClapEditor::new(Rc::new(calls), &name, asked, &alive, Some(bundle.clone()))), &alive)
    });

    Ok(Box::new(ClapPlugin {
        name,
        is_instrument,
        sample_rate,
        audio_in_channel_count,
        audio_out_channel_count,
        latency,
        tempo: None,
        params_ext,
        params_cache,
        param_ids,
        pending_param_changes: Vec::new(),
        preset_cache,
        preset_data,
        preset_load_ext,
        editor_id,
        callback_id,
        _alive: alive,
        _bundle: bundle,
        instance,
        audio_processor: Some(started),
        output_ports,
        output_port_channel_counts,
        output_channel_bufs,
        input_ports,
        input_port_channel_counts,
        input_channel_bufs,
        event_buffer,
    }))
}

/// Find a CLAP plugin by ID or bundle path.
/// Returns (bundle, plugin_id, name, is_instrument).
pub(super) fn find_plugin(source: &str) -> anyhow::Result<(PluginBundle, String, String, bool)> {
    use clack_host::plugin::features::INSTRUMENT;

    // Try stripping "clap:" prefix for ID-based lookup
    if let Some(plugin_id) = source.strip_prefix("clap:") {
        // Search installed bundles for this ID (standard paths + extra paths)
        let all_paths = clack_finder::ClapFinder::from_standard_paths()
            .into_iter()
            .chain(extra_clap_bundles());

        for bundle_path in all_paths {
            let bundle = match unsafe { PluginBundle::load(&bundle_path) } {
                Ok(b) => b,
                Err(_) => continue,
            };
            let factory = match bundle.get_plugin_factory() {
                Some(f) => f,
                None => continue,
            };

            for descriptor in factory.plugin_descriptors() {
                let id = match descriptor.id() {
                    Some(id) => id,
                    None => continue,
                };
                if id.to_str().ok() == Some(plugin_id) {
                    let name = descriptor
                        .name()
                        .map(|s| s.to_string_lossy().to_string())
                        .unwrap_or_else(|| plugin_id.to_string());
                    let is_instrument = descriptor.features().any(|f| f == INSTRUMENT);
                    return Ok((bundle, plugin_id.to_string(), name, is_instrument));
                }
            }
        }
        anyhow::bail!(
            "CLAP plugin not found: {plugin_id}\nRun `tang enumerate plugins` to list available plugins."
        );
    }

    // Direct path to a .clap bundle — load and pick the first plugin
    let path = Path::new(source);
    let bundle = unsafe { PluginBundle::load(path) }
        .map_err(|e| anyhow::anyhow!("Failed to load CLAP bundle {}: {e}", path.display()))?;
    let factory = bundle
        .get_plugin_factory()
        .ok_or_else(|| anyhow::anyhow!("CLAP bundle has no plugin factory: {}", path.display()))?;
    let descriptor = factory
        .plugin_descriptors()
        .next()
        .ok_or_else(|| anyhow::anyhow!("No plugins in CLAP bundle: {}", path.display()))?;

    let id = descriptor
        .id()
        .ok_or_else(|| anyhow::anyhow!("CLAP plugin has no ID"))?
        .to_str()
        .map_err(|_| anyhow::anyhow!("CLAP plugin ID is not valid UTF-8"))?
        .to_string();
    let name = descriptor
        .name()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_else(|| id.clone());
    let is_instrument = descriptor.features().any(|f| f == INSTRUMENT);

    Ok((bundle, id, name, is_instrument))
}

// ---------------------------------------------------------------------------
// Plugin trait implementation
// ---------------------------------------------------------------------------

/// What a plugin is told of time in a block (step 120): the tempo, and nothing else. A position, a loop, a time signature and whether the transport plays are the host's, and a program that is a plugin does not pass them on yet, so the flags
/// say the tempo and no more, and every other field is as a host that has none of them leaves it.
fn transport_event(tempo: f64) -> TransportEvent {
    let zero = BeatTime::from_float(0.0);
    TransportEvent {
        header: EventHeader::new_core(0, EventFlags::empty()),
        flags: TransportFlags::HAS_TEMPO,
        song_pos_beats: zero,
        song_pos_seconds: SecondsTime::from_float(0.0),
        tempo,
        tempo_inc: 0.0,
        loop_start_beats: zero,
        loop_end_beats: zero,
        loop_start_seconds: SecondsTime::from_float(0.0),
        loop_end_seconds: SecondsTime::from_float(0.0),
        bar_start: zero,
        bar_number: 0,
        time_signature_numerator: 4,
        time_signature_denominator: 4,
    }
}

impl Plugin for ClapPlugin {
    fn editor_id(&self) -> Option<editors::EditorId> {
        self.editor_id
    }

    fn name(&self) -> &str {
        &self.name
    }

    fn is_instrument(&self) -> bool {
        self.is_instrument
    }

    fn sample_rate(&self) -> f32 {
        self.sample_rate
    }

    fn audio_input_count(&self) -> usize {
        self.audio_in_channel_count
    }

    fn audio_output_count(&self) -> usize {
        self.audio_out_channel_count
    }

    fn latency(&self) -> u32 {
        self.latency
    }

    fn set_tempo(&mut self, bpm: f32) {
        if bpm.is_finite() && bpm > 0.0 {
            self.tempo = Some(f64::from(bpm));
        }
    }

    fn process(
        &mut self,
        midi_events: &[(u64, [u8; 3])],
        audio_in: &[&[f32]],
        audio_out: &mut [&mut [f32]],
    ) -> anyhow::Result<()> {
        let processor = self
            .audio_processor
            .as_mut()
            .ok_or_else(|| anyhow::anyhow!("CLAP audio processor not active"))?;
        // What the plugin is told of time: the tempo, if it has been told one (a transport on the stack: nothing is made on the heap).
        let transport = self.tempo.map(transport_event);

        let frames = audio_out.first().map(|b| b.len()).unwrap_or(0);
        if frames == 0 {
            return Ok(());
        }

        // Push pending parameter changes into the event buffer
        self.event_buffer.clear();
        for (param_id, value) in self.pending_param_changes.drain(..) {
            let event =
                ParamValueEvent::new(0, param_id, Pckn::match_all(), value, Cookie::empty());
            self.event_buffer.push(&event);
        }

        // Convert MIDI events to clack MidiEvent and push to event buffer
        for (timestamp, bytes) in midi_events {
            let midi =
                clack_host::events::event_types::MidiEvent::new(*timestamp as u32, 0, *bytes);
            self.event_buffer.push(&midi);
            log::debug!("CLAP: pushed MIDI event t={timestamp} data={bytes:02x?}",);
        }

        // Resize per-channel output buffers
        for buf in &mut self.output_channel_bufs {
            buf.resize(frames, 0.0);
            buf.fill(0.0);
        }

        // Build output audio buffers (one port per entry in output_port_channel_counts)
        // Collect all channel slices first, then split into ports to satisfy the borrow checker
        let mut all_slices: Vec<&mut [f32]> = self
            .output_channel_bufs
            .iter_mut()
            .map(|b| b.as_mut_slice())
            .collect();

        let mut remainder = all_slices.as_mut_slice();
        let mut port_buffers: Vec<AudioPortBuffer<_, _>> = Vec::new();
        for &ch_count in &self.output_port_channel_counts {
            let (port_slices, rest) = remainder.split_at_mut(ch_count as usize);
            remainder = rest;
            // Take ownership of the &mut [f32] references out of the slice
            let channel_slices: Vec<&mut [f32]> =
                port_slices.iter_mut().map(|s| &mut **s).collect();
            port_buffers.push(AudioPortBuffer {
                latency: 0,
                channels: AudioPortBufferType::f32_output_only(channel_slices),
            });
        }

        let mut output_audio = self.output_ports.with_output_buffers(port_buffers);

        // Build input audio buffers from audio_in
        // Copy caller's data into our internal buffers
        for (ch, buf) in self.input_channel_bufs.iter_mut().enumerate() {
            buf.resize(frames, 0.0);
            if ch < audio_in.len() {
                let copy_len = buf.len().min(audio_in[ch].len());
                buf[..copy_len].copy_from_slice(&audio_in[ch][..copy_len]);
            } else {
                buf.fill(0.0);
            }
        }

        // Collect input channel slices at the same scope level so they live long enough
        let mut in_slices: Vec<&mut [f32]> = self
            .input_channel_bufs
            .iter_mut()
            .map(|b| b.as_mut_slice())
            .collect();

        let input_events = self.event_buffer.as_input();
        let mut output_events = OutputEvents::void();

        if in_slices.is_empty() {
            let input_audio = InputAudioBuffers::empty();
            processor
                .process(
                    &input_audio,
                    &mut output_audio,
                    &input_events,
                    &mut output_events,
                    None,
                    transport.as_ref(),
                )
                .map_err(|e| anyhow::anyhow!("CLAP process error: {e}"))?;
        } else {
            use clack_host::process::audio_buffers::InputChannel;

            let mut in_remainder = in_slices.as_mut_slice();
            let mut in_port_buffers: Vec<AudioPortBuffer<_, _>> = Vec::new();
            for &ch_count in &self.input_port_channel_counts {
                let (port_slices, rest) = in_remainder.split_at_mut(ch_count as usize);
                in_remainder = rest;
                let channels: Vec<InputChannel<f32>> = port_slices
                    .iter_mut()
                    .map(|s| InputChannel::variable(&mut **s))
                    .collect();
                in_port_buffers.push(AudioPortBuffer {
                    latency: 0,
                    channels: AudioPortBufferType::f32_input_only(channels),
                });
            }

            let input_audio = self.input_ports.with_input_buffers(in_port_buffers);
            processor
                .process(
                    &input_audio,
                    &mut output_audio,
                    &input_events,
                    &mut output_events,
                    None,
                    transport.as_ref(),
                )
                .map_err(|e| anyhow::anyhow!("CLAP process error: {e}"))?;
        }

        // Copy from internal channel buffers to caller's output slices
        for (ch, out_slice) in audio_out.iter_mut().enumerate() {
            if ch < self.output_channel_bufs.len() {
                let src = &self.output_channel_bufs[ch];
                let copy_len = out_slice.len().min(src.len());
                out_slice[..copy_len].copy_from_slice(&src[..copy_len]);
            }
        }

        Ok(())
    }

    fn parameters(&self) -> Vec<ParameterInfo> {
        self.params_cache.clone()
    }

    fn get_parameter(&mut self, index: u32) -> Option<f32> {
        let param_id = *self.param_ids.get(index as usize)?;
        let ext = self.params_ext?;
        let mut handle = self.instance.plugin_handle();
        ext.get_value(&mut handle, param_id).map(|v| v as f32)
    }

    fn set_parameter(&mut self, index: u32, value: f32) -> anyhow::Result<()> {
        let param_id = *self
            .param_ids
            .get(index as usize)
            .ok_or_else(|| anyhow::anyhow!("Parameter index out of range: {index}"))?;
        self.pending_param_changes.push((param_id, value as f64));
        Ok(())
    }

    fn presets(&self) -> Vec<Preset> {
        self.preset_cache.clone()
    }

    fn load_preset(&mut self, id: &str) -> anyhow::Result<()> {
        let index: usize = id
            .parse()
            .map_err(|_| anyhow::anyhow!("Invalid preset ID: {id}"))?;
        let data = self
            .preset_data
            .get(index)
            .ok_or_else(|| anyhow::anyhow!("Preset index out of range: {id}"))?;

        let preset_load = self
            .preset_load_ext
            .ok_or_else(|| anyhow::anyhow!("Plugin does not support preset loading"))?;

        let load_key_cstring = data
            .load_key
            .as_ref()
            .map(|k| std::ffi::CString::new(k.as_str()))
            .transpose()
            .map_err(|_| anyhow::anyhow!("Invalid load key"))?;
        let load_key: Option<&CStr> = load_key_cstring.as_deref();

        let file_path_cstring = data
            .file_path
            .as_ref()
            .map(|p| std::ffi::CString::new(p.as_str()))
            .transpose()
            .map_err(|_| anyhow::anyhow!("Invalid file path"))?;

        let location = if data.is_plugin_location {
            Location::Plugin
        } else {
            match &file_path_cstring {
                Some(path) => Location::File { path },
                None => anyhow::bail!("Preset has no location"),
            }
        };

        preset_load
            .load_from_location(&mut self.instance.plugin_handle(), location, load_key)
            .map_err(|e| anyhow::anyhow!("Failed to load preset: {e}"))?;

        log::info!("CLAP: loaded preset {id}");
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn what_a_plugin_asks_of_its_window_is_written_down_for_the_loop_that_turns_the_window() {
        let shared = TangHostShared::default();
        shared.request_resize(GuiSize { width: 800, height: 500 }).expect("a size is taken");
        assert_eq!(shared.asked.take_resize(), Some((800, 500)), "the width and the height, as they were said");
        assert!(!shared.asked.take_closed(), "nothing has closed");
        shared.closed(true);
        assert!(shared.asked.take_closed(), "a plugin that says that it closed its window");
        // The button of a plugin's own that hides its window is a request to hide it: the window is closed, which hides it.
        shared.request_hide().expect("a window is closed when a plugin asks for it to be hidden");
        assert!(shared.asked.take_closed());
        // A window is opened by a person and not by a plugin: that is said, and nothing else comes of it.
        assert!(shared.request_show().is_err());
        assert_eq!(shared.asked.take_resize(), None);
        assert!(!shared.asked.take_closed());
    }

    #[test]
    fn what_a_plugin_is_told_of_time_is_the_tempo_and_nothing_else() {
        let transport = transport_event(93.5);
        assert_eq!(transport.tempo, 93.5, "the tempo, as it was told");
        assert_eq!(transport.flags, TransportFlags::HAS_TEMPO, "and that is all that the flags say: not that it plays, not a position, not a loop, not a time signature");
        assert_eq!(transport.song_pos_beats.to_float(), 0.0);
        assert_eq!(transport.tempo_inc, 0.0, "a tempo that stands still");
    }

    /// CLAP plugins of the tests' own, run through the program's host as the plugin of a file is (`load_found`): they keep what the host gave them of time in the last block they were run for. One has no audio input, as an
    /// instrument does, and one has an input, as an effect does: the host runs them through its two calls of `process` (with the input's buffers, and without).
    mod probe {
        use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};

        use clack_extensions::audio_ports::{AudioPortFlags, AudioPortInfo, AudioPortInfoWriter, AudioPortType, PluginAudioPorts, PluginAudioPortsImpl};
        use clack_plugin::entry::DefaultPluginFactory;
        use clack_plugin::prelude::*;

        pub const INSTRUMENT_ID: &str = "tang.test.transport-probe";
        pub const EFFECT_ID: &str = "tang.test.transport-probe-effect";
        /// What the last block came with: 0 for no transport at all, and for a transport the bits of its flags and 1 more (so that it is never 0 when there was one).
        pub static SEEN: AtomicU32 = AtomicU32::new(0);
        /// The tempo of that transport, as the bits of the f64.
        pub static TEMPO: AtomicU64 = AtomicU64::new(0);

        /// A probe that is an effect (it declares an audio input) or is not.
        pub struct Probe<const EFFECT: bool>;
        pub type InstrumentEntry = SinglePluginEntry<Probe<false>>;
        pub type EffectEntry = SinglePluginEntry<Probe<true>>;

        /// The plugin's main thread, which has its audio ports to say: a stereo output, and a stereo input if it is an effect.
        pub struct Ports<const EFFECT: bool>;

        impl<const EFFECT: bool> Plugin for Probe<EFFECT> {
            type AudioProcessor<'a> = Processor;
            type Shared<'a> = ();
            type MainThread<'a> = Ports<EFFECT>;

            fn declare_extensions(builder: &mut PluginExtensions<Self>, _shared: Option<&()>) {
                builder.register::<PluginAudioPorts>();
            }
        }

        impl<const EFFECT: bool> DefaultPluginFactory for Probe<EFFECT> {
            fn get_descriptor() -> PluginDescriptor {
                PluginDescriptor::new(if EFFECT { EFFECT_ID } else { INSTRUMENT_ID }, "Transport probe")
            }

            fn new_shared(_host: HostSharedHandle<'_>) -> Result<Self::Shared<'_>, PluginError> {
                Ok(())
            }

            fn new_main_thread<'a>(_host: HostMainThreadHandle<'a>, _shared: &'a Self::Shared<'a>) -> Result<Self::MainThread<'a>, PluginError> {
                Ok(Ports)
            }
        }

        impl<'a, const EFFECT: bool> PluginMainThread<'a, ()> for Ports<EFFECT> {}

        impl<const EFFECT: bool> PluginAudioPortsImpl for Ports<EFFECT> {
            fn count(&mut self, is_input: bool) -> u32 {
                u32::from(!is_input || EFFECT)
            }

            fn get(&mut self, index: u32, is_input: bool, writer: &mut AudioPortInfoWriter) {
                if index == 0 && (!is_input || EFFECT) {
                    writer.set(&AudioPortInfo {
                        id: ClapId::new(u32::from(!is_input)),
                        name: if is_input { b"in" } else { b"out" },
                        channel_count: 2,
                        flags: AudioPortFlags::IS_MAIN,
                        port_type: Some(AudioPortType::STEREO),
                        in_place_pair: None,
                    });
                }
            }
        }

        pub struct Processor;

        impl<'a, const EFFECT: bool> PluginAudioProcessor<'a, (), Ports<EFFECT>> for Processor {
            fn activate(_host: HostAudioProcessorHandle<'a>, _main_thread: &mut Ports<EFFECT>, _shared: &'a (), _config: PluginAudioConfiguration) -> Result<Self, PluginError> {
                Ok(Processor)
            }

            fn process(&mut self, process: Process, _audio: Audio, _events: Events) -> Result<ProcessStatus, PluginError> {
                match process.transport {
                    Some(transport) => {
                        SEEN.store(transport.flags.bits() + 1, Ordering::Relaxed);
                        TEMPO.store(transport.tempo.to_bits(), Ordering::Relaxed);
                    }
                    None => SEEN.store(0, Ordering::Relaxed),
                }
                Ok(ProcessStatus::Continue)
            }
        }
    }

    #[test]
    fn a_clap_plugin_that_the_program_hosts_is_given_the_tempo_that_it_was_told_as_its_transport_and_none_before() {
        use std::sync::atomic::Ordering::Relaxed;

        let instrument = PluginBundle::load_from_clack::<probe::InstrumentEntry>(c"").expect("the entry of the instrument probe loads");
        let effect = PluginBundle::load_from_clack::<probe::EffectEntry>(c"").expect("the entry of the effect probe loads");
        // (Each is run through the host's call of `process` that its audio ports make: the effect through the one that has the input's buffers.)
        for (kind, bundle, id) in [("an instrument", instrument, probe::INSTRUMENT_ID), ("an effect", effect, probe::EFFECT_ID)] {
            let mut plugin = load_found(bundle, id.to_string(), "Transport probe".to_string(), kind == "an instrument", 48_000.0, 256).expect("the host makes the plugin");
            assert_eq!(plugin.audio_input_count(), if kind == "an effect" { 2 } else { 0 }, "{kind} has the input that it says");
            let silence = vec![0.0f32; 256];
            let (mut left, mut right) = (vec![0.0f32; 256], vec![0.0f32; 256]);
            let mut block = |plugin: &mut Box<dyn Plugin>| {
                let mut out: [&mut [f32]; 2] = [&mut left[..], &mut right[..]];
                plugin.process(&[], &[&silence[..], &silence[..]], &mut out).expect("a block is run");
            };
            // A plugin that has not been told a tempo is given no transport at all, as every CLAP plugin was.
            probe::SEEN.store(99, Relaxed);
            block(&mut plugin);
            assert_eq!(probe::SEEN.load(Relaxed), 0, "{kind}: no transport before a tempo is told");
            // Told one, it is given a transport that says the tempo, and that is all it says.
            plugin.set_tempo(93.0);
            block(&mut plugin);
            assert_eq!(probe::SEEN.load(Relaxed), TransportFlags::HAS_TEMPO.bits() + 1, "{kind}: the tempo is there, and no flag but its own");
            assert_eq!(f64::from_bits(probe::TEMPO.load(Relaxed)), 93.0, "{kind}");
            // What is told that is no tempo is not kept: the plugin goes on at the last.
            for bad in [f32::NAN, f32::INFINITY, f32::NEG_INFINITY, 0.0, -1.0] {
                plugin.set_tempo(bad);
                block(&mut plugin);
                assert_eq!(f64::from_bits(probe::TEMPO.load(Relaxed)), 93.0, "{kind}: {bad} is no tempo");
            }
            // And a tempo that changes is the one that it is given in the next block.
            plugin.set_tempo(120.5);
            block(&mut plugin);
            assert_eq!(f64::from_bits(probe::TEMPO.load(Relaxed)), 120.5, "{kind}");
        }
    }

    #[test]
    fn a_plugin_that_asks_for_a_callback_from_any_thread_is_written_down_for_the_main_threads_turn() {
        let shared = TangHostShared::default();
        assert!(!shared.asked.take_callback(), "nobody asked");
        std::thread::scope(|scope| {
            scope.spawn(|| shared.request_callback());
        });
        assert!(shared.asked.take_callback(), "asked on another thread");
        assert!(!shared.asked.take_callback(), "taken: it is one call for an ask");
    }
}
