//! The four chain types, told apart only by which end faces apps as a device.

/// Which side of a chain carries a device `media.class` (ADR 0003).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ChainKind {
    /// Apps play into it (`Audio/Sink`); the other end is linked where the user says.
    Output,
    /// Apps record from it (`Audio/Source`); the other end captures what the user says.
    Mic,
    /// Both ends face apps: play into it, record from it.
    Cable,
    /// Neither end faces apps; both are linked by the user (a chain between chains).
    Insert,
}

impl ChainKind {
    pub const ALL: [ChainKind; 4] = [
        ChainKind::Output,
        ChainKind::Mic,
        ChainKind::Cable,
        ChainKind::Insert,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            ChainKind::Output => "output",
            ChainKind::Mic => "mic",
            ChainKind::Cable => "cable",
            ChainKind::Insert => "insert",
        }
    }

    pub fn parse(text: &str) -> Option<ChainKind> {
        ChainKind::ALL.into_iter().find(|k| k.as_str() == text)
    }

    /// `media.class` of the capture end (what the chain hears).
    pub fn capture_class(self) -> &'static str {
        match self {
            ChainKind::Output | ChainKind::Cable => "Audio/Sink",
            ChainKind::Mic | ChainKind::Insert => "Stream/Input/Audio",
        }
    }

    /// `media.class` of the playback end (what the chain says): always a stream. A mic
    /// or cable chain feeds it into a null source node named after the chain, because
    /// PipeWire 1.6 gives an output stream declared `Audio/Source/Virtual` a single
    /// input port and nothing apps can record from.
    pub fn playback_class(self) -> &'static str {
        "Stream/Output/Audio"
    }

    /// The chain also owns a `support.null-audio-sink` of class `Audio/Source/Virtual`
    /// (named by `source_node_name`), which apps record from.
    pub fn source_device(self) -> bool {
        matches!(self, ChainKind::Mic | ChainKind::Cable)
    }

    /// `node.name` of the null source apps record from: the chain's own name for a mic,
    /// `<name>.source` for a cable, whose own name is already its sink. Two nodes with
    /// one name make every lookup by name (targets, links) pick either.
    pub fn source_node_name(self, name: &str) -> String {
        match self {
            ChainKind::Cable => format!("{name}.source"),
            _ => name.to_owned(),
        }
    }

    /// `node.name` of the capture end: the chain's own name when it is the device.
    pub fn capture_node_name(self, name: &str) -> String {
        match self {
            ChainKind::Output | ChainKind::Cable => name.to_owned(),
            ChainKind::Mic | ChainKind::Insert => format!("{name}.in"),
        }
    }

    /// `node.name` of the playback end: the chain's own name only for a mic.
    pub fn playback_node_name(self, name: &str) -> String {
        format!("{name}.out")
    }

    /// `node.name` of the key input, the sidechain every keyed slot hears.
    pub fn key_node_name(self, name: &str) -> String {
        format!("{name}.key")
    }

    /// `node.name` of the hardware insert's return input.
    pub fn insert_return_node_name(self, name: &str) -> String {
        format!("{name}.insert.in")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn each_kind_has_a_device_end_or_none() {
        assert_eq!(ChainKind::Output.capture_class(), "Audio/Sink");
        assert_eq!(ChainKind::Output.playback_class(), "Stream/Output/Audio");
        assert_eq!(ChainKind::Mic.playback_class(), "Stream/Output/Audio");
        assert!(ChainKind::Mic.source_device() && ChainKind::Cable.source_device());
        assert!(!ChainKind::Output.source_device());
        assert_eq!(ChainKind::Insert.capture_class(), "Stream/Input/Audio");
        assert_eq!(ChainKind::Insert.playback_class(), "Stream/Output/Audio");
    }

    #[test]
    fn the_device_end_keeps_the_chain_name() {
        assert_eq!(
            ChainKind::Output.capture_node_name("canalis.eq"),
            "canalis.eq"
        );
        assert_eq!(
            ChainKind::Output.playback_node_name("canalis.eq"),
            "canalis.eq.out"
        );
        assert_eq!(
            ChainKind::Mic.capture_node_name("canalis.mic"),
            "canalis.mic.in"
        );
        assert_eq!(
            ChainKind::Mic.playback_node_name("canalis.mic"),
            "canalis.mic.out"
        );
        assert_eq!(ChainKind::parse("cable"), Some(ChainKind::Cable));
        assert_eq!(ChainKind::parse("sink"), None);
    }
}
