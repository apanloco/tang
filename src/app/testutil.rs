//! Test helpers shared by the app and TUI tests.

use crate::plugin::chain::GraphCommand;

/// One line per command, for asserting on what a front-end or the loader sent
/// to the audio thread. `GraphCommand` carries boxed plugins, so it has no
/// `Debug`; this prints each command's variant and its scalar fields (plugins by
/// name). Commands nobody asserts on yet show as `<other>`, which still changes
/// an expected list when one shows up unexpectedly.
pub fn describe(cmd: &GraphCommand) -> String {
    use GraphCommand as C;
    match cmd {
        C::AddGroup => "AddGroup".to_string(),
        C::RemoveGroup { group } => format!("RemoveGroup group={group}"),
        C::SetGroupVolume { group, value } => format!("SetGroupVolume group={group} value={value:.3}"),
        C::SetGroupPan { group, value } => format!("SetGroupPan group={group} value={value:.3}"),
        C::SetGroupMix { group, index, value } => {
            format!("SetGroupMix group={group} fx={index} value={value:.3}")
        }
        C::InsertGroupEffect { group, index, effect, mix } => {
            format!("InsertGroupEffect group={group} index={index} plugin={} mix={mix:.3}", effect.name())
        }
        C::RemoveGroupEffect { group, index } => format!("RemoveGroupEffect group={group} index={index}"),
        C::ReorderGroupEffect { group, from, to } => {
            format!("ReorderGroupEffect group={group} from={from} to={to}")
        }
        C::SetGroupParameter { group, index, param_index, value } => {
            format!("SetGroupParameter group={group} fx={index} param={param_index} value={value:.3}")
        }
        C::InsertGroupModulator { group, index, source } => {
            format!("InsertGroupModulator group={group} index={index} source={source:?}")
        }
        C::RemoveGroupModulator { group, index } => {
            format!("RemoveGroupModulator group={group} index={index}")
        }
        C::AddGroupModTarget { group, mod_index, target } => format!(
            "AddGroupModTarget group={group} mod={mod_index} kind={:?} depth={:.3} base={:.3} range=[{:.3}, {:.3}]",
            target.kind, target.depth, target.base_value, target.param_min, target.param_max
        ),
        C::SetGroupModTargetDepth { group, mod_index, target_index, depth } => {
            format!("SetGroupModTargetDepth group={group} mod={mod_index} target={target_index} depth={depth:.3}")
        }

        C::AddInstrument { range } => format!("AddInstrument range={range:?}"),
        C::RemoveInstrument { inst } => format!("RemoveInstrument inst={inst}"),
        C::ClearInstrument { inst } => format!("ClearInstrument inst={inst}"),
        C::SetLaneGroup { inst, group } => format!("SetLaneGroup inst={inst} group={group:?}"),
        C::SetInstrumentRange { inst, range } => format!("SetInstrumentRange inst={inst} range={range:?}"),
        C::SwapInstrument { inst, instrument, inst_buf, remapper } => format!(
            "SwapInstrument inst={inst} plugin={} bufs={} remap={}",
            instrument.name(),
            inst_buf.len(),
            remapper.is_some()
        ),
        C::SetVolume { inst, value } => format!("SetVolume inst={inst} value={value:.3}"),
        C::SetTranspose { inst, semitones } => format!("SetTranspose inst={inst} semitones={semitones}"),
        C::SetParameter { inst, slot, param_index, value } => {
            format!("SetParameter inst={inst} slot={slot} param={param_index} value={value:.3}")
        }
        C::SetMix { inst, slot, value } => format!("SetMix inst={inst} slot={slot} value={value:.3}"),
        C::InsertEffect { inst, index, effect, mix } => {
            format!("InsertEffect inst={inst} index={index} plugin={} mix={mix:.3}", effect.name())
        }
        C::RemoveEffect { inst, index } => format!("RemoveEffect inst={inst} index={index}"),
        C::ReorderEffect { inst, from, to } => format!("ReorderEffect inst={inst} from={from} to={to}"),
        C::InsertModulator { inst, index, source } => {
            format!("InsertModulator inst={inst} index={index} source={source:?}")
        }
        C::RemoveModulator { inst, index } => format!("RemoveModulator inst={inst} index={index}"),
        C::AddModTarget { inst, mod_index, target } => format!(
            "AddModTarget inst={inst} mod={mod_index} kind={:?} depth={:.3} base={:.3} range=[{:.3}, {:.3}]",
            target.kind, target.depth, target.base_value, target.param_min, target.param_max
        ),
        C::SetModulatorRate { inst, mod_index, rate } => {
            format!("SetModulatorRate inst={inst} mod={mod_index} rate={rate:.3}")
        }
        C::SetModTargetDepth { inst, mod_index, target_index, depth } => {
            format!("SetModTargetDepth inst={inst} mod={mod_index} target={target_index} depth={depth:.3}")
        }

        C::SetPattern { inst, pattern, base_note, in_key } => format!(
            "SetPattern inst={inst} events={} length_samples={} base_note={base_note:?} in_key={in_key}",
            pattern.events.len(),
            pattern.length_samples
        ),
        C::SetGlobalBpm { bpm } => format!("SetGlobalBpm bpm={bpm:.1}"),
        C::SetPatternLength { inst, beats } => format!("SetPatternLength inst={inst} beats={beats:.1}"),
        C::SetPatternEnabled { inst, enabled } => format!("SetPatternEnabled inst={inst} enabled={enabled}"),
        C::SetPatternLooping { inst, looping } => format!("SetPatternLooping inst={inst} looping={looping}"),
        _ => "<other>".to_string(),
    }
}
