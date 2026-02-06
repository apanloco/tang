use std::sync::Arc;

use super::{ParameterInfo, Plugin, PluginInfo, Preset};

/// Shared LV2 runtime: one World + Features, created once and reused for all URI-based loads.
/// Avoids re-scanning the entire LV2 plugin directory for each plugin.
pub struct Lv2Runtime {
    world: Arc<livi::World>,
    features: Arc<livi::Features>,
}

impl Lv2Runtime {
    pub fn new(max_block_size: usize) -> Self {
        let world = livi::World::new();
        let features = world.build_features(livi::FeaturesBuilder {
            min_block_length: 1,
            max_block_length: max_block_size,
        });
        Lv2Runtime {
            world: Arc::new(world),
            features,
        }
    }
}

/// Cached port values for a single LV2 preset.
struct Lv2PresetData {
    port_values: Vec<(livi::PortIndex, f32)>,
}

pub struct Lv2Plugin {
    name: String,
    is_instrument: bool,
    #[expect(dead_code)]
    sample_rate: f32,
    #[expect(dead_code)]
    audio_in_count: usize,
    audio_out_count: usize,
    atom_seq_in_count: usize,
    instance: livi::Instance,
    midi_urid: u32,
    event_buf: livi::event::LV2AtomSequence,
    atom_seq_outputs: Vec<livi::event::LV2AtomSequence>,
    control_input_ports: Vec<livi::Port>,
    /// Pre-allocated silence buffers for padding audio inputs (e.g. unconnected sidechains)
    silence_bufs: Vec<Vec<f32>>,
    preset_cache: Vec<Preset>,
    preset_data: Vec<Lv2PresetData>,
}

/// Eagerly discover all presets for a plugin and cache their port values.
fn discover_presets(
    world: &livi::World,
    uri: &str,
    control_input_ports: &[livi::Port],
) -> (Vec<Preset>, Vec<Lv2PresetData>) {
    let lilv_world = world.raw();
    let uri_node = lilv_world.new_uri(uri);
    let lilv_plugin = match lilv_world.plugins().plugin(&uri_node) {
        Some(p) => p,
        None => return (Vec::new(), Vec::new()),
    };

    let preset_class = lilv_world.new_uri("http://lv2plug.in/ns/ext/presets#Preset");
    let label_pred = lilv_world.new_uri("http://www.w3.org/2000/01/rdf-schema#label");
    let port_pred = lilv_world.new_uri("http://lv2plug.in/ns/lv2core#port");
    let symbol_pred = lilv_world.new_uri("http://lv2plug.in/ns/lv2core#symbol");
    let value_pred = lilv_world.new_uri("http://lv2plug.in/ns/ext/presets#value");

    let preset_nodes = match lilv_plugin.related(Some(&preset_class)) {
        Some(nodes) => nodes,
        None => return (Vec::new(), Vec::new()),
    };

    let mut presets = Vec::new();
    let mut data = Vec::new();

    for preset_node in preset_nodes {
        let _ = lilv_world.load_resource(&preset_node);

        let labels = lilv_world.find_nodes(Some(&preset_node), &label_pred, None);
        let name = labels
            .into_iter()
            .next()
            .and_then(|n| n.as_str().map(String::from))
            .unwrap_or_else(|| preset_node.as_uri().unwrap_or("Unknown").to_string());

        let id = preset_node.as_uri().unwrap_or("").to_string();
        if id.is_empty() {
            continue;
        }

        let mut port_values = Vec::new();
        let ports = lilv_world.find_nodes(Some(&preset_node), &port_pred, None);
        for port_node in ports {
            let symbols = lilv_world.find_nodes(Some(&port_node), &symbol_pred, None);
            let symbol = match symbols
                .into_iter()
                .next()
                .and_then(|n| n.as_str().map(String::from))
            {
                Some(s) => s,
                None => continue,
            };

            let values = lilv_world.find_nodes(Some(&port_node), &value_pred, None);
            let value = match values.into_iter().next().and_then(|n| n.as_float()) {
                Some(v) => v,
                None => continue,
            };

            if let Some(port) = control_input_ports.iter().find(|p| p.symbol == symbol) {
                port_values.push((port.index, value));
            }
        }

        presets.push(Preset { name, id });
        data.push(Lv2PresetData { port_values });
    }

    (presets, data)
}

pub fn load(
    source: &str,
    sample_rate: f32,
    max_block_size: usize,
    runtime: Option<&Lv2Runtime>,
) -> anyhow::Result<Box<dyn Plugin>> {
    let (world, features, lv2_plugin) = if let Some(uri) = source.strip_prefix("lv2:") {
        // Load by URI — reuse shared runtime if available
        let (world, features) = match runtime {
            Some(rt) => (rt.world.clone(), rt.features.clone()),
            None => {
                let w = livi::World::new();
                let f = w.build_features(livi::FeaturesBuilder {
                    min_block_length: 1,
                    max_block_length: max_block_size,
                });
                (Arc::new(w), f)
            }
        };
        let plugin = world
            .plugin_by_uri(uri)
            .ok_or_else(|| anyhow::anyhow!("LV2 plugin not found for URI: {uri}\nRun `tang enumerate` to list available plugins."))?;
        (world, features, plugin)
    } else {
        // Load by bundle path — lightweight, only scans one bundle
        let bundle_uri = if source.starts_with("file://") {
            source.to_string()
        } else {
            let abs = std::path::Path::new(source)
                .canonicalize()
                .map_err(|e| anyhow::anyhow!("Cannot resolve path {source}: {e}"))?;
            format!("file://{}/", abs.display())
        };
        let w = livi::World::with_load_bundle(&bundle_uri);
        let f = w.build_features(livi::FeaturesBuilder {
            min_block_length: 1,
            max_block_length: max_block_size,
        });
        let plugin = w
            .iter_plugins()
            .next()
            .ok_or_else(|| anyhow::anyhow!("No plugin found in bundle: {bundle_uri}"))?;
        (Arc::new(w), f, plugin)
    };

    let name = lv2_plugin.name();
    let uri = lv2_plugin.uri();
    let is_instrument = lv2_plugin.is_instrument();
    let port_counts = lv2_plugin.port_counts();
    let audio_in_count = port_counts.audio_inputs;
    let audio_out_count = port_counts.audio_outputs;
    let atom_seq_in_count = port_counts.atom_sequence_inputs;
    let atom_seq_out_count = port_counts.atom_sequence_outputs;

    log::info!(
        "Loaded LV2 plugin: {name} (instrument={is_instrument}, audio_outputs={audio_out_count}, atom_seq_in={}, atom_seq_out={atom_seq_out_count}, control_in={}, control_out={})",
        port_counts.atom_sequence_inputs,
        port_counts.control_inputs,
        port_counts.control_outputs,
    );

    let instance = unsafe {
        lv2_plugin
            .instantiate(features.clone(), sample_rate as f64)
            .map_err(|e| anyhow::anyhow!("Failed to instantiate LV2 plugin: {e:?}"))?
    };

    let control_input_ports: Vec<livi::Port> = lv2_plugin
        .ports_with_type(livi::PortType::ControlInput)
        .collect();

    let midi_urid = features.midi_urid();
    let event_buf = livi::event::LV2AtomSequence::new(&features, 4096);
    let atom_seq_outputs: Vec<livi::event::LV2AtomSequence> = (0..atom_seq_out_count)
        .map(|_| livi::event::LV2AtomSequence::new(&features, 4096))
        .collect();

    // Eagerly cache presets (avoids needing World on the audio thread)
    let (preset_cache, preset_data) = discover_presets(&world, &uri, &control_input_ports);
    log::info!("Cached {} presets for {name}", preset_cache.len());

    // Pre-allocate silence buffers for any audio inputs (resized in process())
    let silence_bufs = (0..audio_in_count).map(|_| Vec::new()).collect();

    Ok(Box::new(Lv2Plugin {
        name,
        is_instrument,
        sample_rate,
        audio_in_count,
        audio_out_count,
        atom_seq_in_count,
        instance,
        midi_urid,
        event_buf,
        atom_seq_outputs,
        control_input_ports,
        silence_bufs,
        preset_cache,
        preset_data,
    }))
}

/// Enumerate all LV2 plugins found on the system.
pub fn enumerate_plugins() -> Vec<PluginInfo> {
    let world = livi::World::new();
    let lilv_world = world.raw();
    let preset_class = lilv_world.new_uri("http://lv2plug.in/ns/ext/presets#Preset");

    world
        .iter_plugins()
        .map(|p| {
            let preset_count = p
                .raw()
                .related(Some(&preset_class))
                .map(|nodes| nodes.count())
                .unwrap_or(0);
            let path = p
                .raw()
                .bundle_uri()
                .as_uri()
                .unwrap_or("")
                .strip_prefix("file://")
                .unwrap_or("")
                .to_string();
            PluginInfo {
                name: p.name(),
                id: p.uri(),
                is_instrument: p.is_instrument(),
                param_count: p.ports_with_type(livi::PortType::ControlInput).count(),
                preset_count,
                path,
            }
        })
        .collect()
}

impl Plugin for Lv2Plugin {
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
        self.audio_in_count
    }

    fn audio_output_count(&self) -> usize {
        self.audio_out_count
    }

    fn process(
        &mut self,
        midi_events: &[(u64, [u8; 3])],
        audio_in: &[&[f32]],
        audio_out: &mut [&mut [f32]],
    ) -> anyhow::Result<()> {
        self.event_buf.clear();
        for (timestamp, bytes) in midi_events {
            match self.event_buf.push_midi_event::<3>(
                *timestamp as i64,
                self.midi_urid,
                bytes,
            ) {
                Ok(()) => log::debug!(
                    "LV2: pushed MIDI event t={timestamp} len={} data={bytes:02x?}",
                    bytes.len()
                ),
                Err(e) => log::debug!(
                    "LV2: failed to push MIDI event: {e:?} data={bytes:02x?}"
                ),
            }
        }

        let sample_count = audio_out.first().map(|b| b.len()).unwrap_or(0);

        // Clear pre-allocated atom sequence output buffers
        for s in self.atom_seq_outputs.iter_mut() {
            s.clear_as_chunk();
        }

        // Build audio input slices, padding with silence for unconnected ports (e.g. sidechains)
        // Copy provided inputs into our silence_bufs, fill the rest with zeros
        for (i, buf) in self.silence_bufs.iter_mut().enumerate() {
            buf.resize(sample_count, 0.0);
            if i < audio_in.len() {
                let copy_len = buf.len().min(audio_in[i].len());
                buf[..copy_len].copy_from_slice(&audio_in[i][..copy_len]);
            } else {
                buf.fill(0.0);
            }
        }
        let padded_inputs: Vec<&[f32]> = self.silence_bufs.iter().map(|b| b.as_slice()).collect();

        // Only pass atom sequence inputs if the plugin has atom sequence input ports
        if self.atom_seq_in_count > 0 {
            let ports = livi::EmptyPortConnections::new()
                .with_atom_sequence_inputs(std::iter::once(&self.event_buf))
                .with_audio_inputs(padded_inputs.into_iter())
                .with_audio_outputs(audio_out.iter_mut().map(|b| &mut **b))
                .with_atom_sequence_outputs(self.atom_seq_outputs.iter_mut());

            unsafe {
                self.instance
                    .run(sample_count, ports)
                    .map_err(|e| anyhow::anyhow!("LV2 run error: {e:?}"))?;
            }
        } else {
            let ports = livi::EmptyPortConnections::new()
                .with_audio_inputs(padded_inputs.into_iter())
                .with_audio_outputs(audio_out.iter_mut().map(|b| &mut **b))
                .with_atom_sequence_outputs(self.atom_seq_outputs.iter_mut());

            unsafe {
                self.instance
                    .run(sample_count, ports)
                    .map_err(|e| anyhow::anyhow!("LV2 run error: {e:?}"))?;
            }
        }

        Ok(())
    }

    fn parameters(&self) -> Vec<ParameterInfo> {
        self.control_input_ports
            .iter()
            .map(|port| ParameterInfo {
                index: port.index.0 as u32,
                name: port.name.clone(),
                min: port.min_value.unwrap_or(0.0),
                max: port.max_value.unwrap_or(1.0),
                default: port.default_value,
            })
            .collect()
    }

    fn get_parameter(&mut self, index: u32) -> Option<f32> {
        self.instance
            .control_input(livi::PortIndex(index as usize))
    }

    fn set_parameter(&mut self, index: u32, value: f32) -> anyhow::Result<()> {
        self.instance
            .set_control_input(livi::PortIndex(index as usize), value)
            .ok_or_else(|| anyhow::anyhow!("Invalid parameter index: {index}"))?;
        Ok(())
    }

    fn presets(&self) -> Vec<Preset> {
        self.preset_cache.clone()
    }

    fn load_preset(&mut self, id: &str) -> anyhow::Result<()> {
        let data = self
            .preset_data
            .iter()
            .zip(self.preset_cache.iter())
            .find(|(_, preset)| preset.id == id)
            .map(|(data, _)| data)
            .ok_or_else(|| anyhow::anyhow!("LV2 preset not found: {id}"))?;

        for &(port_index, value) in &data.port_values {
            self.instance.set_control_input(port_index, value);
        }

        log::info!("LV2: loaded preset {id}");
        Ok(())
    }
}
