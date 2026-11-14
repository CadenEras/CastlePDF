//! Worker side of the shell/worker boundary (ADR-0001): bounded framing,
//! versioned messages, open documents and tile rendering, the request loop
//! run by the `pdf-worker` binary and a client used by the shell to drive
//! that process.

pub mod client;
pub mod documents;
pub mod frame;
pub mod job;
pub mod protocol;
pub mod server;

/// Incremented on any incompatible change of the framing or messages.
/// Version 2: documents, page sizes and tiles. Version 3: whether the
/// document structure was repaired (`Repair`, ADR-0004). Version 4: the
/// `Skipped::FONT_SUBSTITUTED` bit (ADR-0005). Version 5: the text layer of a
/// page (`PageText`, ADR-0006). Version 6: search in a page (`SearchPage`,
/// ADR-0006).
pub const PROTOCOL_VERSION: u16 = 6;
