//! Channel-layout vocabulary and the strict layout adapter.
//!
//! Two orders are distinguished:
//! * **daemon order** - the order the AES67 daemon publishes (SDP/status layout labels). \`LayoutSpec::known\`
//!   returns these, identical to \`daemon.conf layout=\` definitions used by the device daemon.
//! * **ROAR order** - the order ROAR writes its planar output in (BS.2051 speaker order).
//!   \`LayoutSpec::roar\` returns these; the adapter maps ROAR order -> daemon target order.
//!
//! Strict policy (default, and the only implemented mode): the actual output layout is the
//! intersection of target and source channel labels. Source channels outside the target are
//! dropped with a warning; target channels missing from the source are not filled (silence in the
//! renderer, omitted from the published layout). No downmix/upmix is performed in the bridge.

use crate::{PipelineError, Result};

/// Canonical channel identity. Ordering here is irrelevant; layouts carry their own order.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, PartialOrd, Ord)]
pub enum ChannelId {
    FL,
    FR,
    FC,
    LFE,
    LS,
    RS,
    LRS,
    RRS,
    LW,
    RW,
    TFL,
    TFR,
    TBL,
    TBR,
    TSL,
    TSR,
    LTM,
    RTM,
    LFE2,
    CB,
    TC,
}

impl ChannelId {
    /// Canonical spelling used in SDP labels / status (daemon compatible).
    pub const fn canonical(self) -> &'static str {
        match self {
            ChannelId::FL => "L",
            ChannelId::FR => "R",
            ChannelId::FC => "C",
            ChannelId::LFE => "LFE",
            ChannelId::LS => "Ls",
            ChannelId::RS => "Rs",
            ChannelId::LRS => "Lb",
            ChannelId::RRS => "Rb",
            ChannelId::LW => "Lw",
            ChannelId::RW => "Rw",
            ChannelId::TFL => "Tfl",
            ChannelId::TFR => "Tfr",
            ChannelId::TBL => "Tbl",
            ChannelId::TBR => "Tbr",
            ChannelId::TSL => "Tsl",
            ChannelId::TSR => "Tsr",
            ChannelId::LTM => "Ltm",
            ChannelId::RTM => "Rtm",
            ChannelId::LFE2 => "LFE2",
            ChannelId::CB => "Cb",
            ChannelId::TC => "Tc",
        }
    }

    /// Parse a label. Accepts the daemon spellings and the NSP/ROAR spellings (Lrs/Ltr/...).
    pub fn parse(label: &str) -> Option<Self> {
        let s = label.trim().to_ascii_lowercase();
        let id = match s.as_str() {
            "l" | "fl" | "left" | "frontleft" => ChannelId::FL,
            "r" | "fr" | "right" | "frontright" => ChannelId::FR,
            "c" | "fc" | "center" | "frontcenter" => ChannelId::FC,
            "lfe" | "lf" => ChannelId::LFE,
            "ls" | "sl" | "surroundleft" | "sideleft" => ChannelId::LS,
            "rs" | "sr" | "surroundright" | "sideright" => ChannelId::RS,
            "lb" | "bl" | "lrs" | "blr" | "backleft" | "rearsurroundleft" => ChannelId::LRS,
            "rb" | "br" | "rrs" | "brr" | "backright" | "rearsurroundright" => ChannelId::RRS,
            "lw" | "wideleft" => ChannelId::LW,
            "rw" | "wideright" => ChannelId::RW,
            "tfl" | "lft" | "ltf" | "topfrontleft" => ChannelId::TFL,
            "tfr" | "rft" | "rtf" | "topfrontright" => ChannelId::TFR,
            "tbl" | "ltr" | "lrt" | "topbackleft" | "toprearleft" => ChannelId::TBL,
            "tbr" | "rtr" | "rrt" | "topbackright" | "toprearright" => ChannelId::TBR,
            "tsl" | "lts" | "topsurroundleft" | "topsideleft" => ChannelId::TSL,
            "tsr" | "rts" | "topsurroundright" | "topsideight" | "topside right" => ChannelId::TSR,
            "ltm" | "topmiddleleft" => ChannelId::LTM,
            "rtm" | "topmiddleright" => ChannelId::RTM,
            "lfe2" => ChannelId::LFE2,
            "cb" | "bc" | "backcenter" => ChannelId::CB,
            "tc" | "topcenter" => ChannelId::TC,
            _ => return None,
        };
        Some(id)
    }
}

/// An ordered channel layout with a stable id (e.g. \`"7.1.4"\`).
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct LayoutSpec {
    id: String,
    channels: Vec<ChannelId>,
}

impl LayoutSpec {
    pub fn new(id: impl Into<String>, channels: Vec<ChannelId>) -> Self {
        Self { id: id.into(), channels }
    }

    pub fn from_labels(id: impl Into<String>, labels: &[&str]) -> Result<Self> {
        let mut channels = Vec::with_capacity(labels.len());
        for label in labels {
            channels.push(
                ChannelId::parse(label)
                    .ok_or_else(|| PipelineError::Layout(format!("unknown channel label {label:?}")))?,
            );
        }
        Ok(Self { id: id.into(), channels })
    }

    /// One of the nine daemon layouts (daemon.conf layout=), in daemon channel order.
    pub fn known(id: &str) -> Option<Self> {
        let channels: &[ChannelId] = match id {
            "2.0" => &[ChannelId::FL, ChannelId::FR],
            "4.1.2" => &[ChannelId::FL, ChannelId::FR, ChannelId::LFE, ChannelId::LS, ChannelId::RS, ChannelId::TFL, ChannelId::TFR],
            "5.1" => &[ChannelId::FL, ChannelId::FR, ChannelId::FC, ChannelId::LFE, ChannelId::LS, ChannelId::RS],
            "5.1.2" => &[ChannelId::FL, ChannelId::FR, ChannelId::FC, ChannelId::LFE, ChannelId::LS, ChannelId::RS, ChannelId::TFL, ChannelId::TFR],
            "5.1.4" => &[ChannelId::FL, ChannelId::FR, ChannelId::FC, ChannelId::LFE, ChannelId::LS, ChannelId::RS, ChannelId::TFL, ChannelId::TFR, ChannelId::TBL, ChannelId::TBR],
            "7.1" => &[ChannelId::FL, ChannelId::FR, ChannelId::FC, ChannelId::LFE, ChannelId::LS, ChannelId::RS, ChannelId::LRS, ChannelId::RRS],
            "7.1.2" => &[ChannelId::FL, ChannelId::FR, ChannelId::FC, ChannelId::LFE, ChannelId::LS, ChannelId::RS, ChannelId::LRS, ChannelId::RRS, ChannelId::TFL, ChannelId::TFR],
            "7.1.4" => &[ChannelId::FL, ChannelId::FR, ChannelId::FC, ChannelId::LFE, ChannelId::LS, ChannelId::RS, ChannelId::LRS, ChannelId::RRS, ChannelId::TFL, ChannelId::TFR, ChannelId::TBL, ChannelId::TBR],
            "9.1.6" => &[ChannelId::FL, ChannelId::FR, ChannelId::FC, ChannelId::LFE, ChannelId::LS, ChannelId::RS, ChannelId::LRS, ChannelId::RRS, ChannelId::LW, ChannelId::RW, ChannelId::TFL, ChannelId::TFR, ChannelId::TBL, ChannelId::TBR, ChannelId::TSL, ChannelId::TSR],
            _ => return None,
        };
        Some(Self::new(id, channels.to_vec()))
    }

    /// Same layout in ROAR output order (the order \`render::ObjectRenderer\` returns).
    pub fn roar(id: &str) -> Option<Self> {
        let channels: &[ChannelId] = match id {
            "5.1" => &[ChannelId::FL, ChannelId::FR, ChannelId::FC, ChannelId::LFE, ChannelId::LS, ChannelId::RS],
            "5.1.2" => &[ChannelId::FL, ChannelId::FR, ChannelId::FC, ChannelId::LFE, ChannelId::LS, ChannelId::RS, ChannelId::TFL, ChannelId::TFR],
            "5.1.4" => &[ChannelId::FL, ChannelId::FR, ChannelId::FC, ChannelId::LFE, ChannelId::LS, ChannelId::RS, ChannelId::TFL, ChannelId::TFR, ChannelId::TBL, ChannelId::TBR],
            "7.1" => &[ChannelId::FL, ChannelId::FR, ChannelId::FC, ChannelId::LFE, ChannelId::LS, ChannelId::RS, ChannelId::LRS, ChannelId::RRS],
            "7.1.2" => &[ChannelId::FL, ChannelId::FR, ChannelId::FC, ChannelId::LFE, ChannelId::LS, ChannelId::RS, ChannelId::LRS, ChannelId::RRS, ChannelId::TFL, ChannelId::TFR],
            "7.1.4" => &[ChannelId::FL, ChannelId::FR, ChannelId::FC, ChannelId::LFE, ChannelId::LS, ChannelId::RS, ChannelId::LRS, ChannelId::RRS, ChannelId::TFL, ChannelId::TFR, ChannelId::TBL, ChannelId::TBR],
            "9.1.6" => &[ChannelId::FL, ChannelId::FR, ChannelId::FC, ChannelId::LFE, ChannelId::LRS, ChannelId::RRS, ChannelId::LW, ChannelId::RW, ChannelId::LS, ChannelId::RS, ChannelId::TFL, ChannelId::TFR, ChannelId::TBL, ChannelId::TBR, ChannelId::TSL, ChannelId::TSR],
            "9.1.4" => &[ChannelId::FL, ChannelId::FR, ChannelId::FC, ChannelId::LFE, ChannelId::LRS, ChannelId::RRS, ChannelId::LW, ChannelId::RW, ChannelId::LS, ChannelId::RS, ChannelId::TFL, ChannelId::TFR, ChannelId::TBL, ChannelId::TBR],
            _ => return None,
        };
        Some(Self::new(id, channels.to_vec()))
    }

    pub fn id(&self) -> &str {
        &self.id
    }

    pub fn channels(&self) -> &[ChannelId] {
        &self.channels
    }

    pub fn len(&self) -> usize {
        self.channels.len()
    }

    pub fn is_empty(&self) -> bool {
        self.channels.is_empty()
    }

    pub fn index_of(&self, channel: ChannelId) -> Option<usize> {
        self.channels.iter().position(|&c| c == channel)
    }

    pub fn contains(&self, channel: ChannelId) -> bool {
        self.index_of(channel).is_some()
    }

    /// SDP/status label string, e.g. \`"L,R,C,LFE,Ls,Rs"\`.
    pub fn label_csv(&self) -> String {
        self.channels.iter().map(|c| c.canonical()).collect::<Vec<_>>().join(",")
    }
}

/// Result of the strict target/source intersection.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct LayoutIntersection {
    /// The actual published layout: target channels present in source, in target order.
    pub actual: LayoutSpec,
    /// Source channels not present in target (dropped, warning emitted by the adapter).
    pub dropped_source: Vec<ChannelId>,
    /// Target channels not present in source (not filled, warning emitted by the adapter).
    pub missing_target: Vec<ChannelId>,
}

/// Compute the strict intersection of target and source channel labels.
pub fn intersect(target: &LayoutSpec, source: &LayoutSpec) -> LayoutIntersection {
    let actual_channels: Vec<ChannelId> =
        target.channels().iter().copied().filter(|c| source.contains(*c)).collect();
    let dropped_source: Vec<ChannelId> =
        source.channels().iter().copied().filter(|c| !target.contains(*c)).collect();
    let missing_target: Vec<ChannelId> =
        target.channels().iter().copied().filter(|c| !source.contains(*c)).collect();
    let actual_id = match actual_channels.as_slice() {
        c if c == LayoutSpec::known("5.1").map(|l| l.channels().to_vec()).unwrap_or_default() => "5.1".to_string(),
        c if c == LayoutSpec::known("7.1").map(|l| l.channels().to_vec()).unwrap_or_default() => "7.1".to_string(),
        c if c == LayoutSpec::known("5.1.2").map(|l| l.channels().to_vec()).unwrap_or_default() => "5.1.2".to_string(),
        c if c == LayoutSpec::known("7.1.2").map(|l| l.channels().to_vec()).unwrap_or_default() => "7.1.2".to_string(),
        c if c == LayoutSpec::known("5.1.4").map(|l| l.channels().to_vec()).unwrap_or_default() => "5.1.4".to_string(),
        c if c == LayoutSpec::known("7.1.4").map(|l| l.channels().to_vec()).unwrap_or_default() => "7.1.4".to_string(),
        c if c == LayoutSpec::known("9.1.6").map(|l| l.channels().to_vec()).unwrap_or_default() => "9.1.6".to_string(),
        c => format!("custom({}ch:{})", c.len(), c.iter().map(|x| x.canonical()).collect::<Vec<_>>().join(",")),
    };
    LayoutIntersection {
        actual: LayoutSpec::new(actual_id, actual_channels),
        dropped_source,
        missing_target,
    }
}

/// Static layout adapter between a source layout and a target layout.
///
/// Implementations receive planar blocks (one slice per source channel, each \`frames\` long) and
/// write planar output in target order. \`adapt\` returns the actual layout that downstream
/// SDP/status must publish.
pub trait LayoutAdapter {
    fn adapt(
        &mut self,
        src: &LayoutSpec,
        dst: &LayoutSpec,
        input: &[&[f32]],
        out: &mut [&mut [f32]],
    ) -> Result<LayoutSpec>;
}

/// The only implemented adapter: strict label intersection. Reorders by channel id, drops extra
/// source channels, does not fill missing target channels.
#[derive(Default, Clone, Copy, Debug)]
pub struct IntersectionAdapter;

impl IntersectionAdapter {
    pub fn new() -> Self {
        Self
    }
}

impl LayoutAdapter for IntersectionAdapter {
    fn adapt(
        &mut self,
        src: &LayoutSpec,
        dst: &LayoutSpec,
        input: &[&[f32]],
        out: &mut [&mut [f32]],
    ) -> Result<LayoutSpec> {
        if input.len() != src.len() {
            return Err(PipelineError::Layout(format!(
                "adapter input has {} channels but source layout {} has {}",
                input.len(),
                src.id(),
                src.len()
            )));
        }
        let result = intersect(dst, src);
        if !result.dropped_source.is_empty() {
            log::warn!(
                "layout drop: target {} does not carry source channels {:?}; dropped (no downmix)",
                dst.id(),
                result.dropped_source
            );
        }
        if !result.missing_target.is_empty() {
            log::warn!(
                "layout missing: source {} lacks target channels {:?}; not filled",
                src.id(),
                result.missing_target
            );
        }
        if out.len() < result.actual.len() {
            return Err(PipelineError::Layout(format!(
                "adapter output has {} channels but actual layout {} needs {}",
                out.len(),
                result.actual.id(),
                result.actual.len()
            )));
        }
        let frames = input.first().map(|c| c.len()).unwrap_or(0);
        for (dst_idx, &channel) in result.actual.channels().iter().enumerate() {
            let src_idx = src
                .index_of(channel)
                .ok_or_else(|| PipelineError::Layout(format!("actual channel {channel:?} not in source")))?;
            if input[src_idx].len() != frames {
                return Err(PipelineError::Layout("adapter input channel lengths differ".into()));
            }
            if out[dst_idx].len() < frames {
                return Err(PipelineError::Layout("adapter output channel too short".into()));
            }
            out[dst_idx][..frames].copy_from_slice(&input[src_idx][..frames]);
        }
        Ok(result.actual)
    }
}

/// Reserved layout mode. Only \`Strict\` is implemented; the other values are reserved for future
/// upmix/downmix adapters and are rejected instead of silently enabled.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum LayoutMode {
    #[default]
    Strict,
    Upmix,
    Downmix,
    Auto,
}

impl LayoutMode {
    pub fn from_config(value: &str) -> Result<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "strict" => Ok(LayoutMode::Strict),
            other => Err(PipelineError::Unsupported(format!(
                "layout_mode={other:?} is reserved and not implemented; only \"strict\" is accepted"
            ))),
        }
    }
}

/// Placeholder for a future upmix adapter. NOT implemented, NOT registered in the default chain.
pub trait UpmixAdapter: LayoutAdapter {}

/// Placeholder for a future downmix adapter. NOT implemented, NOT registered in the default chain.
pub trait DownmixAdapter: LayoutAdapter {}

#[cfg(test)]
mod tests {
    use super::*;

    fn ids(spec: &LayoutSpec) -> Vec<&'static str> {
        spec.channels().iter().map(|c| c.canonical()).collect()
    }

    #[test]
    fn known_layouts_parse_and_round_trip() {
        for id in ["2.0", "4.1.2", "5.1", "5.1.2", "5.1.4", "7.1", "7.1.2", "7.1.4", "9.1.6"] {
            let layout = LayoutSpec::known(id).unwrap();
            let reparsed = LayoutSpec::from_labels(id, &layout.channels().iter().map(|c| c.canonical()).collect::<Vec<_>>()).unwrap();
            assert_eq!(layout, reparsed, "layout {id}");
        }
        assert_eq!(LayoutSpec::known("5.1").unwrap().len(), 6);
        assert_eq!(LayoutSpec::known("7.1.4").unwrap().len(), 12);
        assert_eq!(LayoutSpec::known("9.1.6").unwrap().len(), 16);
    }

    #[test]
    fn roar_order_reorders_916_but_not_714() {
        assert_eq!(
            LayoutSpec::roar("7.1.4").unwrap().channels(),
            LayoutSpec::known("7.1.4").unwrap().channels()
        );
        let roar = LayoutSpec::roar("9.1.6").unwrap();
        assert_eq!(roar.channels()[0], ChannelId::FL);
        assert_eq!(roar.channels()[6], ChannelId::LW);
        assert_eq!(roar.channels()[14], ChannelId::TSL);
        let daemon = LayoutSpec::known("9.1.6").unwrap();
        assert_eq!(daemon.channels()[8], ChannelId::LW);
        assert_ne!(roar.channels(), daemon.channels());
    }

    #[test]
    fn strict_intersection_three_spec_cases() {
        // target 5.1.2 + source 5.1 -> actual 5.1
        let r = intersect(&LayoutSpec::known("5.1.2").unwrap(), &LayoutSpec::known("5.1").unwrap());
        assert_eq!(r.actual.id(), "5.1");
        assert_eq!(ids(&r.actual), ["L", "R", "C", "LFE", "Ls", "Rs"]);
        assert!(r.dropped_source.is_empty());
        assert_eq!(r.missing_target, vec![ChannelId::TFL, ChannelId::TFR]);

        // target 7.1.2 + source 7.1 -> actual 7.1
        let r = intersect(&LayoutSpec::known("7.1.2").unwrap(), &LayoutSpec::known("7.1").unwrap());
        assert_eq!(r.actual.id(), "7.1");
        assert_eq!(r.missing_target, vec![ChannelId::TFL, ChannelId::TFR]);
        assert!(r.dropped_source.is_empty());

        // target 7.1.2 + source 5.1 -> actual 5.1
        let r = intersect(&LayoutSpec::known("7.1.2").unwrap(), &LayoutSpec::known("5.1").unwrap());
        assert_eq!(r.actual.id(), "5.1");
        assert_eq!(r.actual.len(), 6);
        assert_eq!(r.missing_target.len(), 4);
        assert!(r.dropped_source.is_empty());
    }

    #[test]
    fn extra_source_channels_are_dropped_not_downmixed() {
        // target 5.1 + source 7.1.4 -> actual 5.1, 7 source channels dropped
        let r = intersect(&LayoutSpec::known("5.1").unwrap(), &LayoutSpec::known("7.1.4").unwrap());
        assert_eq!(r.actual.id(), "5.1");
        assert_eq!(r.actual.channels(), LayoutSpec::known("5.1").unwrap().channels());
        assert_eq!(r.dropped_source.len(), 6); // Lb Rb Tfl Tfr Tbl Tbr
        assert!(r.missing_target.is_empty());
        // 4.1.2 target with 5.1 source: drops C, keeps LFE (label semantics only, no downmix)
        let r = intersect(&LayoutSpec::known("4.1.2").unwrap(), &LayoutSpec::known("5.1").unwrap());
        assert_eq!(r.actual.len(), 5);
        assert_eq!(r.dropped_source, vec![ChannelId::FC]);
    }

    #[test]
    fn adapter_reorders_roar_916_to_daemon_order() {
        let src = LayoutSpec::roar("9.1.6").unwrap();
        let dst = LayoutSpec::known("9.1.6").unwrap();
        let frames = 8usize;
        // input[i][..] = channel index marker
        let input_data: Vec<Vec<f32>> = (0..src.len()).map(|c| vec![c as f32; frames]).collect();
        let input: Vec<&[f32]> = input_data.iter().map(|v| v.as_slice()).collect();
        let mut out_data: Vec<Vec<f32>> = (0..dst.len()).map(|_| vec![-1.0; frames]).collect();
        let mut out: Vec<&mut [f32]> = out_data.iter_mut().map(|v| v.as_mut_slice()).collect();
        let actual = IntersectionAdapter::new().adapt(&src, &dst, &input, &mut out).unwrap();
        assert_eq!(actual.id(), "9.1.6");
        // ROAR 9.1.6 order: L,R,C,LFE,Lrs,Rrs,Lw,Rw,Ls,Rs,Tfl,Tfr,Tbl,Tbr,Tsl,Tsr
        assert_eq!(out_data[0][0], 0.0);   // L at src index 0
        assert_eq!(out_data[8][0], 6.0);   // Lw at src index 6
        assert_eq!(out_data[14][0], 14.0); // Tsl at src index 14
        assert_eq!(out_data[15][0], 15.0); // Tsr at src index 15
    }

    #[test]
    fn adapter_drops_extra_source_channels_and_flags_missing() {
        let src = LayoutSpec::known("7.1.4").unwrap();
        let dst = LayoutSpec::known("5.1.2").unwrap();
        let frames = 4usize;
        let input_data: Vec<Vec<f32>> = (0..src.len()).map(|c| vec![c as f32; frames]).collect();
        let input: Vec<&[f32]> = input_data.iter().map(|v| v.as_slice()).collect();
        let mut out_data: Vec<Vec<f32>> = (0..dst.len()).map(|_| vec![0.0; frames]).collect();
        let mut out: Vec<&mut [f32]> = out_data.iter_mut().map(|v| v.as_mut_slice()).collect();
        let actual = IntersectionAdapter::new().adapt(&src, &dst, &input, &mut out).unwrap();
        assert_eq!(actual.id(), "5.1.2");
        assert_eq!(actual.len(), 8);
        assert_eq!(out_data[0][0], 0.0); // L
        assert_eq!(out_data[6][0], 8.0); // Tfl is source channel 8
    }

    #[test]
    fn layout_mode_only_accepts_strict() {
        assert_eq!(LayoutMode::from_config("strict").unwrap(), LayoutMode::Strict);
        assert!(LayoutMode::from_config("upmix").is_err());
        assert!(LayoutMode::from_config("downmix").is_err());
        assert!(LayoutMode::from_config("auto").is_err());
    }
}
