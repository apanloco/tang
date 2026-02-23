use std::cell::UnsafeCell;
use std::ffi::{c_char, c_void};
use std::path::{Path, PathBuf};

use vst3::Steinberg::Vst::BusDirections_::{kInput, kOutput};
use vst3::Steinberg::Vst::Event_::EventTypes_::{kNoteOffEvent, kNoteOnEvent};
use vst3::Steinberg::Vst::MediaTypes_::{kAudio, kEvent};
use vst3::Steinberg::Vst::ParameterInfo_::ParameterFlags_::kIsProgramChange;
use vst3::Steinberg::Vst::ProcessContext_::StatesAndFlags_::kPlaying;
use vst3::Steinberg::Vst::ProcessContext_::StatesAndFlags_::kTempoValid;
use vst3::Steinberg::Vst::ProcessModes_::kRealtime;
use vst3::Steinberg::Vst::SpeakerArr::{kMono, kStereo};
use vst3::Steinberg::Vst::SymbolicSampleSizes_::kSample32;
use vst3::Steinberg::Vst::{
    AudioBusBuffers, AudioBusBuffers__type0, BusInfo, Event, Event__type0, IAudioProcessor,
    IAudioProcessorTrait as _, IComponent, IComponentHandler, IComponentHandlerTrait,
    IComponentTrait as _, IConnectionPoint, IConnectionPointTrait as _, IEditController,
    IEditControllerTrait as _, IEventList, IEventListTrait, IHostApplication,
    IHostApplicationTrait, IMidiMapping, IMidiMappingTrait as _, IParamValueQueue,
    IParamValueQueueTrait, IParameterChanges, IParameterChangesTrait, IUnitInfo,
    IUnitInfoTrait as _, NoteOffEvent, NoteOnEvent, ParameterInfo as Vst3ParameterInfo,
    ProcessContext, ProcessData, ProcessSetup, ProgramListInfo, String128,
};
use vst3::Steinberg::{
    self, FUnknown, IPluginBaseTrait as _, IPluginFactory, IPluginFactory2,
    IPluginFactory2Trait as _, IPluginFactoryTrait as _, PClassInfo, PClassInfo2, kResultOk,
};
use vst3::{Class, ComPtr, ComWrapper, Interface};

use super::{ParameterInfo, Plugin, PluginInfo, Preset};

// ---------------------------------------------------------------------------
// String helpers
// ---------------------------------------------------------------------------

fn string128_to_string(s: &String128) -> String {
    let end = s.iter().position(|&c| c == 0).unwrap_or(s.len());
    String::from_utf16_lossy(&s[..end])
}

fn string_to_string128(s: &str) -> String128 {
    let mut buf: String128 = [0u16; 128];
    for (i, ch) in s.encode_utf16().take(127).enumerate() {
        buf[i] = ch;
    }
    buf
}

fn char_array_to_string(s: &[c_char]) -> String {
    let end = s.iter().position(|&c| c == 0).unwrap_or(s.len());
    s[..end].iter().map(|&c| c as u8 as char).collect()
}

/// Convert a `Guid` ([u8; 16]) to a TUID ([c_char; 16]) for passing to createInstance.
fn guid_to_tuid(guid: &vst3::com_scrape_types::Guid) -> Steinberg::TUID {
    let mut tuid: Steinberg::TUID = [0; 16];
    for i in 0..16 {
        tuid[i] = guid[i] as c_char;
    }
    tuid
}

// ---------------------------------------------------------------------------
// Platform-specific paths
// ---------------------------------------------------------------------------

fn vst3_search_paths() -> Vec<PathBuf> {
    let mut paths = Vec::new();

    #[cfg(target_os = "linux")]
    {
        if let Some(home) = std::env::var_os("HOME") {
            paths.push(PathBuf::from(home).join(".vst3"));
        }
        paths.push(PathBuf::from("/usr/lib/vst3"));
        paths.push(PathBuf::from("/usr/local/lib/vst3"));
    }

    #[cfg(target_os = "macos")]
    {
        if let Some(home) = std::env::var_os("HOME") {
            paths.push(PathBuf::from(home).join("Library/Audio/Plug-Ins/VST3"));
        }
        paths.push(PathBuf::from("/Library/Audio/Plug-Ins/VST3"));
    }

    #[cfg(target_os = "windows")]
    {
        if let Some(local) = std::env::var_os("LOCALAPPDATA") {
            paths.push(PathBuf::from(local).join("Programs/Common/VST3"));
        }
        paths.push(PathBuf::from(r"C:\Program Files\Common Files\VST3"));
    }

    paths
}

/// Resolve a .vst3 bundle to its platform-specific shared library path.
fn bundle_binary_path(bundle: &Path) -> PathBuf {
    #[cfg(target_os = "linux")]
    {
        let stem = bundle
            .file_stem()
            .unwrap_or_default()
            .to_string_lossy()
            .to_string();
        bundle
            .join("Contents")
            .join("x86_64-linux")
            .join(format!("{stem}.so"))
    }

    #[cfg(target_os = "macos")]
    {
        let stem = bundle
            .file_stem()
            .unwrap_or_default()
            .to_string_lossy()
            .to_string();
        bundle.join("Contents").join("MacOS").join(stem)
    }

    #[cfg(target_os = "windows")]
    {
        let stem = bundle
            .file_stem()
            .unwrap_or_default()
            .to_string_lossy()
            .to_string();
        bundle
            .join("Contents")
            .join("x86_64-win")
            .join(format!("{stem}.vst3"))
    }
}

/// Recursively find all .vst3 bundles under the given directory.
fn find_vst3_bundles(dir: &Path) -> Vec<PathBuf> {
    let mut bundles = Vec::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(current) = stack.pop() {
        let entries = match std::fs::read_dir(&current) {
            Ok(e) => e,
            Err(_) => continue,
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                if path
                    .extension()
                    .is_some_and(|ext| ext.eq_ignore_ascii_case("vst3"))
                {
                    bundles.push(path);
                } else {
                    stack.push(path);
                }
            }
        }
    }
    bundles.sort();
    bundles
}

// ---------------------------------------------------------------------------
// Module loading
// ---------------------------------------------------------------------------

struct Vst3Module {
    factory: Option<ComPtr<IPluginFactory>>,
    #[cfg(target_os = "linux")]
    exit_fn: Option<libloading::Symbol<'static, unsafe extern "C" fn() -> bool>>,
    #[cfg(target_os = "macos")]
    exit_fn: Option<libloading::Symbol<'static, unsafe extern "C" fn() -> bool>>,
    #[cfg(target_os = "windows")]
    exit_fn: Option<libloading::Symbol<'static, unsafe extern "C" fn() -> bool>>,
    // SAFETY: Library must be dropped after factory and exit_fn.
    // Rust drops fields in declaration order, so this is correct.
    _library: libloading::Library,
}

impl Vst3Module {
    fn load(bundle_path: &Path) -> anyhow::Result<Self> {
        let binary = bundle_binary_path(bundle_path);
        if !binary.exists() {
            anyhow::bail!("VST3 binary not found: {}", binary.display());
        }

        // Safety: loading external dynamic libraries is inherently unsafe
        let library = unsafe { libloading::Library::new(&binary) }.map_err(|e| {
            anyhow::anyhow!("Failed to load VST3 library {}: {e}", binary.display())
        })?;

        // Call platform entry function
        #[cfg(target_os = "linux")]
        {
            let entry: libloading::Symbol<unsafe extern "C" fn(*mut c_void) -> bool> =
                unsafe { library.get(b"ModuleEntry") }
                    .map_err(|e| anyhow::anyhow!("ModuleEntry not found: {e}"))?;
            let ok = unsafe { entry(std::ptr::null_mut()) };
            if !ok {
                anyhow::bail!("ModuleEntry returned false");
            }
        }
        #[cfg(target_os = "macos")]
        {
            let entry: libloading::Symbol<unsafe extern "C" fn(*mut c_void) -> bool> =
                unsafe { library.get(b"bundleEntry") }
                    .map_err(|e| anyhow::anyhow!("bundleEntry not found: {e}"))?;
            let ok = unsafe { entry(std::ptr::null_mut()) };
            if !ok {
                anyhow::bail!("bundleEntry returned false");
            }
        }
        #[cfg(target_os = "windows")]
        {
            if let Ok(entry) = unsafe { library.get::<unsafe extern "C" fn() -> bool>(b"InitDll") }
            {
                let ok = unsafe { entry() };
                if !ok {
                    anyhow::bail!("InitDll returned false");
                }
            }
        }

        // Get exit function (stored for Drop)
        // SAFETY: We transmute the lifetime of the Symbol to 'static because
        // we guarantee _library outlives exit_fn (field drop order).
        #[cfg(target_os = "linux")]
        let exit_fn: Option<libloading::Symbol<'static, unsafe extern "C" fn() -> bool>> = unsafe {
            library
                .get::<unsafe extern "C" fn() -> bool>(b"ModuleExit")
                .ok()
                .map(|s| std::mem::transmute(s))
        };
        #[cfg(target_os = "macos")]
        let exit_fn: Option<libloading::Symbol<'static, unsafe extern "C" fn() -> bool>> = unsafe {
            library
                .get::<unsafe extern "C" fn() -> bool>(b"bundleExit")
                .ok()
                .map(|s| std::mem::transmute(s))
        };
        #[cfg(target_os = "windows")]
        let exit_fn: Option<libloading::Symbol<'static, unsafe extern "C" fn() -> bool>> = unsafe {
            library
                .get::<unsafe extern "C" fn() -> bool>(b"ExitDll")
                .ok()
                .map(|s| std::mem::transmute(s))
        };

        // Get plugin factory
        let get_factory: libloading::Symbol<unsafe extern "C" fn() -> *mut IPluginFactory> =
            unsafe { library.get(b"GetPluginFactory") }
                .map_err(|e| anyhow::anyhow!("GetPluginFactory not found: {e}"))?;
        let factory_ptr = unsafe { get_factory() };
        let factory = unsafe { ComPtr::from_raw(factory_ptr) }
            .ok_or_else(|| anyhow::anyhow!("GetPluginFactory returned null"))?;

        Ok(Vst3Module {
            factory: Some(factory),
            exit_fn,
            _library: library,
        })
    }

    fn factory(&self) -> &ComPtr<IPluginFactory> {
        self.factory.as_ref().expect("factory already dropped")
    }
}

impl Drop for Vst3Module {
    fn drop(&mut self) {
        // Drop factory first to release COM references
        self.factory.take();
        // Call exit function
        if let Some(ref exit) = self.exit_fn {
            unsafe {
                exit();
            }
        }
        // _library drops last (unloads the .so/.dylib/.dll)
    }
}

// ---------------------------------------------------------------------------
// Host COM objects
// ---------------------------------------------------------------------------

struct TangHostApp;

impl Class for TangHostApp {
    type Interfaces = (IHostApplication,);
}

impl IHostApplicationTrait for TangHostApp {
    unsafe fn getName(&self, name: *mut String128) -> Steinberg::tresult {
        unsafe { *name = string_to_string128("tang") };
        kResultOk
    }

    unsafe fn createInstance(
        &self,
        _cid: *mut Steinberg::TUID,
        _iid: *mut Steinberg::TUID,
        _obj: *mut *mut c_void,
    ) -> Steinberg::tresult {
        vst3::Steinberg::kNotImplemented
    }
}

struct TangComponentHandler;

impl Class for TangComponentHandler {
    type Interfaces = (IComponentHandler,);
}

impl IComponentHandlerTrait for TangComponentHandler {
    unsafe fn beginEdit(&self, _id: vst3::Steinberg::Vst::ParamID) -> Steinberg::tresult {
        kResultOk
    }

    unsafe fn performEdit(
        &self,
        _id: vst3::Steinberg::Vst::ParamID,
        _value_normalized: vst3::Steinberg::Vst::ParamValue,
    ) -> Steinberg::tresult {
        kResultOk
    }

    unsafe fn endEdit(&self, _id: vst3::Steinberg::Vst::ParamID) -> Steinberg::tresult {
        kResultOk
    }

    unsafe fn restartComponent(&self, _flags: Steinberg::int32) -> Steinberg::tresult {
        log::debug!("VST3 plugin requested restart (ignored)");
        kResultOk
    }
}

// ---------------------------------------------------------------------------
// Process-time COM objects
// ---------------------------------------------------------------------------

struct TangEventList {
    events: UnsafeCell<Vec<Event>>,
}

impl Class for TangEventList {
    type Interfaces = (IEventList,);
}

impl IEventListTrait for TangEventList {
    unsafe fn getEventCount(&self) -> Steinberg::int32 {
        unsafe { (*self.events.get()).len() as Steinberg::int32 }
    }

    unsafe fn getEvent(&self, index: Steinberg::int32, e: *mut Event) -> Steinberg::tresult {
        unsafe {
            let events = &*self.events.get();
            if let Some(event) = events.get(index as usize) {
                *e = *event;
                kResultOk
            } else {
                vst3::Steinberg::kResultFalse
            }
        }
    }

    unsafe fn addEvent(&self, _e: *mut Event) -> Steinberg::tresult {
        vst3::Steinberg::kResultFalse
    }
}

struct TangParamValueQueue {
    param_id: UnsafeCell<u32>,
    value: UnsafeCell<f64>,
}

impl Class for TangParamValueQueue {
    type Interfaces = (IParamValueQueue,);
}

impl IParamValueQueueTrait for TangParamValueQueue {
    unsafe fn getParameterId(&self) -> vst3::Steinberg::Vst::ParamID {
        unsafe { *self.param_id.get() }
    }

    unsafe fn getPointCount(&self) -> Steinberg::int32 {
        1
    }

    unsafe fn getPoint(
        &self,
        index: Steinberg::int32,
        sample_offset: *mut Steinberg::int32,
        value: *mut vst3::Steinberg::Vst::ParamValue,
    ) -> Steinberg::tresult {
        if index == 0 {
            unsafe {
                *sample_offset = 0;
                *value = *self.value.get();
            }
            kResultOk
        } else {
            vst3::Steinberg::kResultFalse
        }
    }

    unsafe fn addPoint(
        &self,
        _sample_offset: Steinberg::int32,
        value: vst3::Steinberg::Vst::ParamValue,
        index: *mut Steinberg::int32,
    ) -> Steinberg::tresult {
        // Accept the point (store latest value), but we only track one point
        unsafe {
            *self.value.get() = value;
            if !index.is_null() {
                *index = 0;
            }
        }
        kResultOk
    }
}

const MAX_PARAM_QUEUES: usize = 64;

struct TangParameterChanges {
    count: UnsafeCell<i32>,
    queues: Vec<ComWrapper<TangParamValueQueue>>,
}

impl Class for TangParameterChanges {
    type Interfaces = (IParameterChanges,);
}

impl IParameterChangesTrait for TangParameterChanges {
    unsafe fn getParameterCount(&self) -> Steinberg::int32 {
        unsafe { *self.count.get() }
    }

    unsafe fn getParameterData(&self, index: Steinberg::int32) -> *mut IParamValueQueue {
        if (index as usize) < (unsafe { *self.count.get() } as usize) {
            self.queues
                .get(index as usize)
                .and_then(|q| q.as_com_ref::<IParamValueQueue>())
                .map(|r| r.as_ptr())
                .unwrap_or(std::ptr::null_mut())
        } else {
            std::ptr::null_mut()
        }
    }

    unsafe fn addParameterData(
        &self,
        id: *const vst3::Steinberg::Vst::ParamID,
        index: *mut Steinberg::int32,
    ) -> *mut IParamValueQueue {
        unsafe {
            let count = *self.count.get();
            if (count as usize) < self.queues.len() {
                *self.queues[count as usize].param_id.get() = *id;
                *self.queues[count as usize].value.get() = 0.0;
                *self.count.get() = count + 1;
                if !index.is_null() {
                    *index = count;
                }
                self.queues[count as usize]
                    .as_com_ref::<IParamValueQueue>()
                    .map(|r| r.as_ptr())
                    .unwrap_or(std::ptr::null_mut())
            } else {
                std::ptr::null_mut()
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Vst3Plugin
// ---------------------------------------------------------------------------

pub struct Vst3Plugin {
    name: String,
    is_instrument: bool,
    sample_rate: f32,
    audio_in_channel_count: usize,
    audio_out_channel_count: usize,
    separate_controller: bool,
    params_cache: Vec<ParameterInfo>,
    param_ids: Vec<u32>,
    pending_param_changes: Vec<(u32, f64)>,
    preset_cache: Vec<Preset>,
    preset_param_id: Option<u32>,
    preset_count: usize,
    // Pre-allocated audio buffers
    output_bufs: Vec<Vec<f32>>,
    input_bufs: Vec<Vec<f32>>,
    // Process-time COM objects (pre-allocated, reused each process() call)
    param_changes: ComWrapper<TangParameterChanges>,
    output_param_changes: ComWrapper<TangParameterChanges>,
    event_list: ComWrapper<TangEventList>,
    // MIDI CC → parameter mapping (index = CC number, 128 = pitch bend)
    cc_param_map: Vec<Option<u32>>,
    // Connection points (for disconnect on Drop)
    comp_connection: Option<ComPtr<IConnectionPoint>>,
    ctrl_connection: Option<ComPtr<IConnectionPoint>>,
    // COM pointers into the loaded library — must drop before _module.
    // Rust drops fields in declaration order, so these must come before _module
    // to ensure Release() calls go through valid vtable pointers.
    component: ComPtr<IComponent>,
    processor: ComPtr<IAudioProcessor>,
    controller: ComPtr<IEditController>,
    _handler: ComWrapper<TangComponentHandler>,
    _host_app: ComWrapper<TangHostApp>,
    // SAFETY: _module must be the last field. It unloads the shared library on
    // drop, so all ComPtrs referencing objects from the library must drop first.
    _module: Vst3Module,
}

// Safety: Same justification as CLAP — the plugin is created, activated, and
// preset-loaded on the main thread, then moved into the audio callback closure.
// Only process() is called on the audio thread. On shutdown the plugin is sent
// back to the main thread for deactivation and drop.
unsafe impl Send for Vst3Plugin {}

impl Drop for Vst3Plugin {
    fn drop(&mut self) {
        unsafe {
            self.processor.setProcessing(0);
            self.component.setActive(0);
            // Disconnect before terminating
            if let (Some(cc), Some(tc)) = (&self.comp_connection, &self.ctrl_connection) {
                cc.disconnect(tc.as_ptr());
                tc.disconnect(cc.as_ptr());
            }
            if self.separate_controller {
                self.controller.terminate();
            }
            self.component.terminate();
        }
    }
}

// ---------------------------------------------------------------------------
// MIDI → VST3 event conversion
// ---------------------------------------------------------------------------

fn make_note_on(channel: i16, pitch: i16, velocity: f32, sample_offset: i32) -> Event {
    Event {
        busIndex: 0,
        sampleOffset: sample_offset,
        ppqPosition: 0.0,
        flags: 0,
        r#type: kNoteOnEvent as u16,
        __field0: Event__type0 {
            noteOn: NoteOnEvent {
                channel,
                pitch,
                tuning: 0.0,
                velocity,
                length: 0,
                noteId: -1,
            },
        },
    }
}

fn make_note_off(channel: i16, pitch: i16, velocity: f32, sample_offset: i32) -> Event {
    Event {
        busIndex: 0,
        sampleOffset: sample_offset,
        ppqPosition: 0.0,
        flags: 0,
        r#type: kNoteOffEvent as u16,
        __field0: Event__type0 {
            noteOff: NoteOffEvent {
                channel,
                pitch,
                velocity,
                noteId: -1,
                tuning: 0.0,
            },
        },
    }
}

// ---------------------------------------------------------------------------
// Plugin trait implementation
// ---------------------------------------------------------------------------

impl Plugin for Vst3Plugin {
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

    fn process(
        &mut self,
        midi_events: &[(u64, [u8; 3])],
        audio_in: &[&[f32]],
        audio_out: &mut [&mut [f32]],
    ) -> anyhow::Result<()> {
        let frames = audio_out.first().map(|b| b.len()).unwrap_or(0);
        if frames == 0 {
            return Ok(());
        }

        // Populate event list with MIDI note events
        let events = unsafe { &mut *self.event_list.events.get() };
        events.clear();

        // Populate parameter changes
        let param_changes_count = unsafe { &mut *self.param_changes.count.get() };
        *param_changes_count = 0;
        // Reset output parameter changes (plugin writes into this)
        unsafe { *self.output_param_changes.count.get() = 0 };
        let mut queue_idx = 0usize;

        // Queue pending parameter changes from set_parameter()
        for &(param_id, value) in &self.pending_param_changes {
            if queue_idx < MAX_PARAM_QUEUES {
                unsafe {
                    *self.param_changes.queues[queue_idx].param_id.get() = param_id;
                    *self.param_changes.queues[queue_idx].value.get() = value;
                }
                queue_idx += 1;
            }
        }
        self.pending_param_changes.clear();

        // Convert MIDI events
        for &(timestamp, bytes) in midi_events {
            let status = bytes[0] & 0xF0;
            let channel = (bytes[0] & 0x0F) as i16;
            let sample_offset = timestamp as i32;

            match status {
                0x90 if bytes[2] > 0 => {
                    let velocity = bytes[2] as f32 / 127.0;
                    events.push(make_note_on(
                        channel,
                        bytes[1] as i16,
                        velocity,
                        sample_offset,
                    ));
                    log::debug!(
                        "VST3: note on ch={channel} pitch={} vel={}",
                        bytes[1],
                        bytes[2]
                    );
                }
                0x80 | 0x90 => {
                    let velocity = bytes[2] as f32 / 127.0;
                    events.push(make_note_off(
                        channel,
                        bytes[1] as i16,
                        velocity,
                        sample_offset,
                    ));
                    log::debug!(
                        "VST3: note off ch={channel} pitch={} vel={}",
                        bytes[1],
                        bytes[2]
                    );
                }
                0xE0 => {
                    // Pitch bend → parameter change via MIDI mapping
                    if let Some(param_id) = self.cc_param_map.get(128).copied().flatten() {
                        let bend = ((bytes[2] as u16) << 7 | bytes[1] as u16) as f64 / 16383.0;
                        if queue_idx < MAX_PARAM_QUEUES {
                            unsafe {
                                *self.param_changes.queues[queue_idx].param_id.get() = param_id;
                                *self.param_changes.queues[queue_idx].value.get() = bend;
                            }
                            queue_idx += 1;
                        }
                    }
                }
                0xB0 => {
                    // CC → parameter change via MIDI mapping
                    let cc = bytes[1] as usize;
                    if let Some(param_id) = self.cc_param_map.get(cc).copied().flatten() {
                        let value = bytes[2] as f64 / 127.0;
                        if queue_idx < MAX_PARAM_QUEUES {
                            unsafe {
                                *self.param_changes.queues[queue_idx].param_id.get() = param_id;
                                *self.param_changes.queues[queue_idx].value.get() = value;
                            }
                            queue_idx += 1;
                        }
                    }
                }
                _ => {}
            }
        }

        *param_changes_count = queue_idx as i32;

        // Prepare audio buffers
        for buf in &mut self.output_bufs {
            buf.resize(frames, 0.0);
            buf.fill(0.0);
        }
        for (ch, buf) in self.input_bufs.iter_mut().enumerate() {
            buf.resize(frames, 0.0);
            if ch < audio_in.len() {
                let copy_len = buf.len().min(audio_in[ch].len());
                buf[..copy_len].copy_from_slice(&audio_in[ch][..copy_len]);
            } else {
                buf.fill(0.0);
            }
        }

        // Build channel pointer arrays
        let mut output_ptrs: Vec<*mut f32> = self
            .output_bufs
            .iter_mut()
            .map(|b| b.as_mut_ptr())
            .collect();
        let mut input_ptrs: Vec<*mut f32> =
            self.input_bufs.iter_mut().map(|b| b.as_mut_ptr()).collect();

        let mut output_bus = AudioBusBuffers {
            numChannels: self.audio_out_channel_count as i32,
            silenceFlags: 0,
            __field0: AudioBusBuffers__type0 {
                channelBuffers32: output_ptrs.as_mut_ptr(),
            },
        };

        let mut input_bus = AudioBusBuffers {
            numChannels: self.audio_in_channel_count as i32,
            silenceFlags: 0,
            __field0: AudioBusBuffers__type0 {
                channelBuffers32: input_ptrs.as_mut_ptr(),
            },
        };

        let param_changes_ptr = self
            .param_changes
            .as_com_ref::<IParameterChanges>()
            .unwrap()
            .as_ptr();
        let event_list_ptr = self.event_list.as_com_ref::<IEventList>().unwrap().as_ptr();

        let has_audio_input = self.audio_in_channel_count > 0;

        let mut context: ProcessContext = unsafe { std::mem::zeroed() };
        context.state = kPlaying | kTempoValid;
        context.sampleRate = self.sample_rate as f64;
        context.tempo = 120.0;

        let mut process_data = ProcessData {
            processMode: kRealtime as i32,
            symbolicSampleSize: kSample32 as i32,
            numSamples: frames as i32,
            numInputs: if has_audio_input { 1 } else { 0 },
            numOutputs: 1,
            inputs: if has_audio_input {
                &mut input_bus
            } else {
                std::ptr::null_mut()
            },
            outputs: &mut output_bus,
            inputParameterChanges: param_changes_ptr,
            outputParameterChanges: self
                .output_param_changes
                .as_com_ref::<IParameterChanges>()
                .unwrap()
                .as_ptr(),
            inputEvents: event_list_ptr,
            outputEvents: std::ptr::null_mut(),
            processContext: &mut context,
        };

        let result = unsafe { self.processor.process(&mut process_data) };
        if result != kResultOk {
            log::warn!("VST3 process returned {result}");
        }

        // Copy output to caller's buffers
        for (ch, out_slice) in audio_out.iter_mut().enumerate() {
            if ch < self.output_bufs.len() {
                let src = &self.output_bufs[ch];
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
        let normalized = unsafe { self.controller.getParamNormalized(param_id) };
        let plain = unsafe { self.controller.normalizedParamToPlain(param_id, normalized) };
        Some(plain as f32)
    }

    fn set_parameter(&mut self, index: u32, value: f32) -> anyhow::Result<()> {
        let param_id = *self
            .param_ids
            .get(index as usize)
            .ok_or_else(|| anyhow::anyhow!("Parameter index out of range: {index}"))?;
        let normalized = unsafe {
            self.controller
                .plainParamToNormalized(param_id, value as f64)
        };
        self.pending_param_changes.push((param_id, normalized));
        Ok(())
    }

    fn presets(&self) -> Vec<Preset> {
        self.preset_cache.clone()
    }

    fn load_preset(&mut self, id: &str) -> anyhow::Result<()> {
        let index: usize = id
            .parse()
            .map_err(|_| anyhow::anyhow!("Invalid preset ID: {id}"))?;
        let preset_param_id = self
            .preset_param_id
            .ok_or_else(|| anyhow::anyhow!("Plugin does not support program changes"))?;

        if self.preset_count == 0 {
            anyhow::bail!("No presets available");
        }

        // Set program change parameter: normalized value = index / (count - 1)
        let normalized = if self.preset_count == 1 {
            0.0
        } else {
            index as f64 / (self.preset_count - 1) as f64
        };

        unsafe {
            self.controller
                .setParamNormalized(preset_param_id, normalized);
        }
        self.pending_param_changes
            .push((preset_param_id, normalized));

        log::info!("VST3: loaded preset {id}");
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Loading
// ---------------------------------------------------------------------------

pub fn load(
    source: &str,
    sample_rate: f32,
    max_block_size: usize,
) -> anyhow::Result<Box<dyn Plugin>> {
    let (module, class_cid, name, is_instrument) = find_plugin(source)?;

    let host_app = ComWrapper::new(TangHostApp);
    let handler = ComWrapper::new(TangComponentHandler);

    // Get host context as FUnknown pointer
    let host_unknown: ComPtr<FUnknown> = host_app
        .to_com_ptr::<FUnknown>()
        .ok_or_else(|| anyhow::anyhow!("Failed to get FUnknown from host app"))?;

    let factory = module.factory();

    // Create IComponent
    let component: ComPtr<IComponent> = {
        let iid = guid_to_tuid(&<IComponent as Interface>::IID);
        let mut obj: *mut c_void = std::ptr::null_mut();
        let result = unsafe { factory.createInstance(class_cid.as_ptr(), iid.as_ptr(), &mut obj) };
        if result != kResultOk || obj.is_null() {
            anyhow::bail!("Failed to create VST3 component (result={result})");
        }
        unsafe { ComPtr::from_raw(obj as *mut IComponent) }
            .ok_or_else(|| anyhow::anyhow!("createInstance returned null IComponent"))?
    };

    // Initialize component
    let result = unsafe { component.initialize(host_unknown.as_ptr()) };
    if result != kResultOk {
        anyhow::bail!("IComponent::initialize failed (result={result})");
    }

    // Get IAudioProcessor from component
    let processor: ComPtr<IAudioProcessor> = component
        .cast::<IAudioProcessor>()
        .ok_or_else(|| anyhow::anyhow!("Component does not implement IAudioProcessor"))?;

    // Get IEditController — try single-component first, then separate
    let (controller, separate_controller): (ComPtr<IEditController>, bool) =
        if let Some(ec) = component.cast::<IEditController>() {
            log::info!("VST3: single-component design (component implements IEditController)");
            (ec, false)
        } else {
            let mut controller_cid: Steinberg::TUID = [0; 16];
            let result = unsafe { component.getControllerClassId(&mut controller_cid) };
            if result != kResultOk {
                anyhow::bail!("Failed to get controller class ID (result={result})");
            }

            let iid = guid_to_tuid(&<IEditController as Interface>::IID);
            let mut obj: *mut c_void = std::ptr::null_mut();
            let result =
                unsafe { factory.createInstance(controller_cid.as_ptr(), iid.as_ptr(), &mut obj) };
            if result != kResultOk || obj.is_null() {
                anyhow::bail!("Failed to create separate IEditController (result={result})");
            }
            let ec = unsafe { ComPtr::from_raw(obj as *mut IEditController) }
                .ok_or_else(|| anyhow::anyhow!("createInstance returned null controller"))?;

            let result = unsafe { ec.initialize(host_unknown.as_ptr()) };
            if result != kResultOk {
                anyhow::bail!("IEditController::initialize failed (result={result})");
            }

            log::info!("VST3: separate controller design");
            (ec, true)
        };

    // Set component handler
    let handler_ptr = handler
        .to_com_ptr::<IComponentHandler>()
        .ok_or_else(|| anyhow::anyhow!("Failed to get IComponentHandler from handler"))?;
    unsafe {
        controller.setComponentHandler(handler_ptr.as_ptr());
    }

    // Connect component and controller via IConnectionPoint (if separate)
    let (comp_connection, ctrl_connection) = if separate_controller {
        let comp_conn = component.cast::<IConnectionPoint>();
        let ctrl_conn = controller.cast::<IConnectionPoint>();
        if let (Some(cc), Some(tc)) = (&comp_conn, &ctrl_conn) {
            unsafe {
                cc.connect(tc.as_ptr());
                tc.connect(cc.as_ptr());
            }
            log::info!("VST3: connected component and controller via IConnectionPoint");
        }
        (comp_conn, ctrl_conn)
    } else {
        (None, None)
    };

    // Set bus arrangements (stereo)
    let mut input_arr: vst3::Steinberg::Vst::SpeakerArrangement = kStereo;
    let mut output_arr: vst3::Steinberg::Vst::SpeakerArrangement = kStereo;

    // Query bus counts to determine arrangement
    let audio_out_bus_count = unsafe { component.getBusCount(kAudio as i32, kOutput as i32) };
    let audio_in_bus_count = unsafe { component.getBusCount(kAudio as i32, kInput as i32) };

    // Query output bus info
    let audio_out_channel_count = if audio_out_bus_count > 0 {
        let mut info: BusInfo = unsafe { std::mem::zeroed() };
        let result = unsafe { component.getBusInfo(kAudio as i32, kOutput as i32, 0, &mut info) };
        if result == kResultOk {
            log::info!(
                "VST3 audio output bus 0: channels={}, name={}",
                info.channelCount,
                string128_to_string(&info.name),
            );
            output_arr = match info.channelCount {
                1 => kMono,
                _ => kStereo,
            };
            info.channelCount as usize
        } else {
            2
        }
    } else {
        2
    };

    // Query input bus info
    let audio_in_channel_count = if audio_in_bus_count > 0 {
        let mut info: BusInfo = unsafe { std::mem::zeroed() };
        let result = unsafe { component.getBusInfo(kAudio as i32, kInput as i32, 0, &mut info) };
        if result == kResultOk {
            log::info!(
                "VST3 audio input bus 0: channels={}, name={}",
                info.channelCount,
                string128_to_string(&info.name),
            );
            input_arr = match info.channelCount {
                1 => kMono,
                _ => kStereo,
            };
            info.channelCount as usize
        } else {
            0
        }
    } else {
        0
    };

    // Set bus arrangements
    if audio_in_bus_count > 0 {
        unsafe {
            processor.setBusArrangements(&mut input_arr, 1, &mut output_arr, 1);
        }
    } else {
        unsafe {
            processor.setBusArrangements(std::ptr::null_mut(), 0, &mut output_arr, 1);
        }
    }

    // Activate buses
    if audio_out_bus_count > 0 {
        unsafe {
            component.activateBus(kAudio as i32, kOutput as i32, 0, 1);
        }
    }
    if audio_in_bus_count > 0 {
        unsafe {
            component.activateBus(kAudio as i32, kInput as i32, 0, 1);
        }
    }
    // Activate event input bus (for MIDI)
    let event_in_bus_count = unsafe { component.getBusCount(kEvent as i32, kInput as i32) };
    if event_in_bus_count > 0 {
        unsafe {
            component.activateBus(kEvent as i32, kInput as i32, 0, 1);
        }
    }

    // Query parameters
    let param_count = unsafe { controller.getParameterCount() };
    let mut params_cache = Vec::with_capacity(param_count as usize);
    let mut param_ids = Vec::with_capacity(param_count as usize);
    let mut preset_param_id: Option<u32> = None;

    for i in 0..param_count {
        let mut info: Vst3ParameterInfo = unsafe { std::mem::zeroed() };
        let result = unsafe { controller.getParameterInfo(i, &mut info) };
        if result != kResultOk {
            continue;
        }

        // Check for program change parameter
        if info.flags & kIsProgramChange != 0 {
            preset_param_id = Some(info.id);
            // Don't expose program change as a regular parameter
            continue;
        }

        let name_str = string128_to_string(&info.title);
        let default_normalized = info.defaultNormalizedValue;
        let min = unsafe { controller.normalizedParamToPlain(info.id, 0.0) } as f32;
        let max = unsafe { controller.normalizedParamToPlain(info.id, 1.0) } as f32;
        let default =
            unsafe { controller.normalizedParamToPlain(info.id, default_normalized) } as f32;

        let param_index = params_cache.len() as u32;
        param_ids.push(info.id);
        params_cache.push(ParameterInfo {
            index: param_index,
            name: name_str,
            min,
            max,
            default,
        });
    }
    log::info!("VST3 plugin has {} parameters", params_cache.len());

    // Discover presets via IUnitInfo
    let mut preset_cache = Vec::new();
    let mut preset_count = 0usize;

    if let Some(unit_info) = controller.cast::<IUnitInfo>() {
        let list_count = unsafe { unit_info.getProgramListCount() };
        for list_idx in 0..list_count {
            let mut list_info: ProgramListInfo = unsafe { std::mem::zeroed() };
            let result = unsafe { unit_info.getProgramListInfo(list_idx, &mut list_info) };
            if result != kResultOk {
                continue;
            }

            let count = list_info.programCount;
            for prog_idx in 0..count {
                let mut name_buf: String128 = [0u16; 128];
                let result =
                    unsafe { unit_info.getProgramName(list_info.id, prog_idx, &mut name_buf) };
                if result == kResultOk {
                    let preset_name = string128_to_string(&name_buf);
                    let id = preset_cache.len().to_string();
                    preset_cache.push(Preset {
                        name: preset_name,
                        id,
                    });
                }
            }
        }
        preset_count = preset_cache.len();
    }
    log::info!("VST3 plugin has {} presets", preset_count);

    // Query MIDI CC → parameter mapping
    let mut cc_param_map: Vec<Option<u32>> = vec![None; 130]; // 0-127 CC + 128 pitch bend + 129 aftertouch
    if let Some(mapping) = controller.cast::<IMidiMapping>() {
        for cc in 0i16..130 {
            let mut param_id: u32 = 0;
            let result = unsafe { mapping.getMidiControllerAssignment(0, 0, cc, &mut param_id) };
            if result == kResultOk {
                cc_param_map[cc as usize] = Some(param_id);
                log::debug!("VST3 MIDI mapping: CC{cc} → param {param_id}");
            }
        }
    }

    log::info!(
        "Loaded VST3 plugin: {name} (instrument={is_instrument}, \
         output_channels={audio_out_channel_count}, params={}, presets={})",
        params_cache.len(),
        preset_count,
    );

    // Setup processing
    let mut setup = ProcessSetup {
        processMode: kRealtime as i32,
        symbolicSampleSize: kSample32 as i32,
        maxSamplesPerBlock: max_block_size as i32,
        sampleRate: sample_rate as f64,
    };
    let result = unsafe { processor.setupProcessing(&mut setup) };
    if result != kResultOk {
        log::warn!("VST3 setupProcessing returned {result}");
    }

    // Activate and start processing
    let result = unsafe { component.setActive(1) };
    if result != kResultOk {
        anyhow::bail!("IComponent::setActive(true) failed (result={result})");
    }
    let result = unsafe { processor.setProcessing(1) };
    if result != kResultOk {
        log::warn!("VST3 setProcessing returned {result}");
    }

    // Pre-allocate buffers
    let output_bufs: Vec<Vec<f32>> = (0..audio_out_channel_count).map(|_| Vec::new()).collect();
    let input_bufs: Vec<Vec<f32>> = (0..audio_in_channel_count).map(|_| Vec::new()).collect();

    // Pre-allocate process-time COM objects
    let param_changes = ComWrapper::new(TangParameterChanges {
        count: UnsafeCell::new(0),
        queues: (0..MAX_PARAM_QUEUES)
            .map(|_| {
                ComWrapper::new(TangParamValueQueue {
                    param_id: UnsafeCell::new(0),
                    value: UnsafeCell::new(0.0),
                })
            })
            .collect(),
    });
    let output_param_changes = ComWrapper::new(TangParameterChanges {
        count: UnsafeCell::new(0),
        queues: (0..MAX_PARAM_QUEUES)
            .map(|_| {
                ComWrapper::new(TangParamValueQueue {
                    param_id: UnsafeCell::new(0),
                    value: UnsafeCell::new(0.0),
                })
            })
            .collect(),
    });
    let event_list = ComWrapper::new(TangEventList {
        events: UnsafeCell::new(Vec::with_capacity(256)),
    });

    Ok(Box::new(Vst3Plugin {
        name,
        is_instrument,
        sample_rate,
        audio_in_channel_count,
        audio_out_channel_count,
        _module: module,
        component,
        processor,
        controller,
        _handler: handler,
        _host_app: host_app,
        separate_controller,
        params_cache,
        param_ids,
        pending_param_changes: Vec::new(),
        preset_cache,
        preset_param_id,
        preset_count,
        output_bufs,
        input_bufs,
        param_changes,
        output_param_changes,
        event_list,
        cc_param_map,
        comp_connection,
        ctrl_connection,
    }))
}

/// Find a VST3 plugin by name or bundle path.
/// Returns (module, class_cid, name, is_instrument).
fn find_plugin(source: &str) -> anyhow::Result<(Vst3Module, Steinberg::TUID, String, bool)> {
    // Try stripping "vst3:" prefix for name-based lookup
    if let Some(plugin_name) = source.strip_prefix("vst3:") {
        let search_name = plugin_name.to_lowercase();
        for search_dir in vst3_search_paths() {
            if !search_dir.exists() {
                continue;
            }
            for bundle_path in find_vst3_bundles(&search_dir) {
                if let Ok((module, cid, name, is_instrument)) =
                    scan_bundle_for_name(&bundle_path, &search_name)
                {
                    return Ok((module, cid, name, is_instrument));
                }
            }
        }
        anyhow::bail!(
            "VST3 plugin not found: {plugin_name}\n\
             Run `tang enumerate plugins` to list available plugins."
        );
    }

    // Direct path to a .vst3 bundle
    let path = Path::new(source);
    let module = Vst3Module::load(path)?;
    let factory = module.factory();

    let count = unsafe { factory.countClasses() };
    if count == 0 {
        anyhow::bail!("No classes in VST3 bundle: {}", path.display());
    }

    // Pick the first Audio Module Class
    for i in 0..count {
        let mut info: PClassInfo = unsafe { std::mem::zeroed() };
        let result = unsafe { factory.getClassInfo(i, &mut info) };
        if result != kResultOk {
            continue;
        }

        let category = char_array_to_string(&info.category);
        if category != "Audio Module Class" {
            continue;
        }

        let name = char_array_to_string(&info.name);
        let is_instrument = is_class_instrument(factory, i);

        return Ok((module, info.cid, name, is_instrument));
    }

    anyhow::bail!(
        "No Audio Module Class found in VST3 bundle: {}",
        path.display()
    );
}

fn scan_bundle_for_name(
    bundle_path: &Path,
    search_name: &str,
) -> anyhow::Result<(Vst3Module, Steinberg::TUID, String, bool)> {
    let module = Vst3Module::load(bundle_path)?;
    let factory = module.factory();
    let count = unsafe { factory.countClasses() };

    for i in 0..count {
        let mut info: PClassInfo = unsafe { std::mem::zeroed() };
        let result = unsafe { factory.getClassInfo(i, &mut info) };
        if result != kResultOk {
            continue;
        }

        let category = char_array_to_string(&info.category);
        if category != "Audio Module Class" {
            continue;
        }

        let name = char_array_to_string(&info.name);
        if name.to_lowercase().contains(search_name) {
            let is_instrument = is_class_instrument(factory, i);
            return Ok((module, info.cid, name, is_instrument));
        }
    }

    anyhow::bail!("No matching class in {}", bundle_path.display());
}

/// Check if a class is an instrument by examining subCategories from IPluginFactory2.
fn is_class_instrument(factory: &ComPtr<IPluginFactory>, index: Steinberg::int32) -> bool {
    if let Some(f2) = factory.cast::<IPluginFactory2>() {
        let mut info2: PClassInfo2 = unsafe { std::mem::zeroed() };
        let result = unsafe { f2.getClassInfo2(index, &mut info2) };
        if result == kResultOk {
            let subcats = char_array_to_string(&info2.subCategories);
            return subcats.contains("Instrument");
        }
    }
    false
}

// ---------------------------------------------------------------------------
// Enumeration
// ---------------------------------------------------------------------------

pub fn enumerate_plugins() -> Vec<PluginInfo> {
    let mut plugins = Vec::new();

    for search_dir in vst3_search_paths() {
        if !search_dir.exists() {
            continue;
        }
        for bundle_path in find_vst3_bundles(&search_dir) {
            match scan_bundle_for_enum(&bundle_path) {
                Some(found) => plugins.extend(found),
                None => {
                    log::warn!("Failed to scan VST3 bundle: {}", bundle_path.display());
                }
            }
        }
    }

    plugins
}

fn scan_bundle_for_enum(bundle_path: &Path) -> Option<Vec<PluginInfo>> {
    let module = Vst3Module::load(bundle_path).ok()?;
    let factory = module.factory();
    let count = unsafe { factory.countClasses() };

    let mut found = Vec::new();
    for i in 0..count {
        let mut info: PClassInfo = unsafe { std::mem::zeroed() };
        let result = unsafe { factory.getClassInfo(i, &mut info) };
        if result != kResultOk {
            continue;
        }

        let category = char_array_to_string(&info.category);
        if category != "Audio Module Class" {
            continue;
        }

        let name = char_array_to_string(&info.name);
        let is_instrument = is_class_instrument(factory, i);

        // Briefly instantiate to query param count and preset count
        let (param_count, preset_count) =
            brief_instantiate(factory, &info.cid, &module).unwrap_or((0, 0));

        found.push(PluginInfo {
            name: name.clone(),
            id: name,
            is_instrument,
            param_count,
            preset_count,
            path: bundle_path.to_string_lossy().to_string(),
        });
    }

    Some(found)
}

/// Briefly instantiate a VST3 plugin to query parameter and preset counts.
fn brief_instantiate(
    factory: &ComPtr<IPluginFactory>,
    class_cid: &Steinberg::TUID,
    _module: &Vst3Module,
) -> Option<(usize, usize)> {
    let host_app = ComWrapper::new(TangHostApp);
    let host_unknown: ComPtr<FUnknown> = host_app.to_com_ptr::<FUnknown>()?;

    let iid = guid_to_tuid(&<IComponent as Interface>::IID);
    let mut obj: *mut c_void = std::ptr::null_mut();
    let result = unsafe { factory.createInstance(class_cid.as_ptr(), iid.as_ptr(), &mut obj) };
    if result != kResultOk || obj.is_null() {
        return None;
    }
    let component = unsafe { ComPtr::from_raw(obj as *mut IComponent) }?;
    let init_result = unsafe { component.initialize(host_unknown.as_ptr()) };
    if init_result != kResultOk {
        return None;
    }

    // Get controller
    let (controller, separate) = if let Some(ec) = component.cast::<IEditController>() {
        (ec, false)
    } else {
        let mut ctrl_cid: Steinberg::TUID = [0; 16];
        unsafe { component.getControllerClassId(&mut ctrl_cid) };
        let ctrl_iid = guid_to_tuid(&<IEditController as Interface>::IID);
        let mut ctrl_obj: *mut c_void = std::ptr::null_mut();
        let r =
            unsafe { factory.createInstance(ctrl_cid.as_ptr(), ctrl_iid.as_ptr(), &mut ctrl_obj) };
        if r != kResultOk || ctrl_obj.is_null() {
            unsafe { component.terminate() };
            return None;
        }
        let ec = unsafe { ComPtr::from_raw(ctrl_obj as *mut IEditController) }?;
        unsafe { ec.initialize(host_unknown.as_ptr()) };
        (ec, true)
    };

    // Connect component and controller for separate-controller plugins
    let (comp_conn, ctrl_conn) = if separate {
        let cc = component.cast::<IConnectionPoint>();
        let tc = controller.cast::<IConnectionPoint>();
        if let (Some(cc), Some(tc)) = (&cc, &tc) {
            unsafe {
                cc.connect(tc.as_ptr());
                tc.connect(cc.as_ptr());
            }
        }
        (cc, tc)
    } else {
        (None, None)
    };

    let param_count = unsafe { controller.getParameterCount() } as usize;

    // Count presets via IUnitInfo
    let preset_count = if let Some(unit_info) = controller.cast::<IUnitInfo>() {
        let list_count = unsafe { unit_info.getProgramListCount() };
        let mut total = 0usize;
        for list_idx in 0..list_count {
            let mut list_info: ProgramListInfo = unsafe { std::mem::zeroed() };
            let r = unsafe { unit_info.getProgramListInfo(list_idx, &mut list_info) };
            if r == kResultOk {
                total += list_info.programCount as usize;
            }
        }
        total
    } else {
        0
    };

    // Clean up — disconnect before terminate, drop connection points before controller
    if let (Some(cc), Some(tc)) = (&comp_conn, &ctrl_conn) {
        unsafe {
            cc.disconnect(tc.as_ptr());
            tc.disconnect(cc.as_ptr());
        }
    }
    drop(comp_conn);
    drop(ctrl_conn);
    if separate {
        unsafe { controller.terminate() };
    }
    drop(controller);
    unsafe { component.terminate() };

    Some((param_count, preset_count))
}
