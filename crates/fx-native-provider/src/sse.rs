//! Incremental server-sent-events framing.
//!
//! Strict on purpose: invalid UTF-8 is reported, never skipped, so a damaged stream cannot
//! look like a clean one. Decoding the data of an event belongs to the wire format.

/// One framed event.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct Frame {
    /// The `event:` name, when the stream gave one.
    pub(crate) event: Option<String>,
    /// The `data:` payload (several data lines are joined with a newline).
    pub(crate) data: String,
}

/// Splits a byte stream into events.
#[derive(Default)]
pub(crate) struct Framer {
    buffer: Vec<u8>,
    event: Option<String>,
    data: Vec<String>,
}

impl Framer {
    /// Adds bytes and returns the events they completed.
    pub(crate) fn push(&mut self, bytes: &[u8]) -> Result<Vec<Frame>, String> {
        self.buffer.extend_from_slice(bytes);
        let mut out = Vec::new();
        while let Some(end) = self.buffer.iter().position(|b| *b == b'\n') {
            let line: Vec<u8> = self.buffer.drain(..=end).collect();
            let line = std::str::from_utf8(&line[..line.len() - 1])
                .map_err(|_| "the stream is not valid UTF-8".to_owned())?;
            let line = line.strip_suffix('\r').unwrap_or(line);
            if line.is_empty() {
                if !self.data.is_empty() {
                    out.push(Frame {
                        event: self.event.take(),
                        data: self.data.join("\n"),
                    });
                    self.data.clear();
                }
                self.event = None;
            } else if let Some(value) = line.strip_prefix("data:") {
                self.data
                    .push(value.strip_prefix(' ').unwrap_or(value).to_owned());
            } else if let Some(value) = line.strip_prefix("event:") {
                self.event = Some(value.trim().to_owned());
            }
            // Comments, `id:` and `retry:` carry nothing this adapter uses.
        }
        Ok(out)
    }
}
