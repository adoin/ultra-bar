//! Bounded JSON-lines transport for API v3 resident plugins. Stdout is protocol only.
use serde::{Deserialize, Serialize};
use std::io::{self, BufRead, Write};
pub const MAX_LINE: usize = 64 * 1024;
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum HostEvent {
    Initialize {
        protocol: u32,
        id: String,
        data_dir: String,
        accent: String,
    },
    Show,
    Foreground {
        executable: String,
        title: String,
    },
    Theme {
        accent: String,
    },
    Shutdown,
}
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Page {
    pub id: String,
    pub text: String,
    pub tooltip: String,
    /// Remaining fraction, not consumed fraction. None means unknown.
    pub progress: Option<f64>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum PluginEvent {
    Ready,
    Widget {
        pages: Vec<Page>,
    },
    Open {
        path: String,
        #[serde(default)]
        select_parent: bool,
    },
}
pub fn read_line(reader: &mut impl BufRead) -> io::Result<Option<String>> {
    let mut bytes = Vec::new();
    loop {
        let chunk = reader.fill_buf()?;
        if chunk.is_empty() {
            return if bytes.is_empty() {
                Ok(None)
            } else {
                Err(io::ErrorKind::UnexpectedEof.into())
            };
        }
        let end = chunk.iter().position(|b| *b == b'\n');
        let length = end.map_or(chunk.len(), |i| i + 1);
        if bytes.len() + length > MAX_LINE {
            return Err(io::ErrorKind::InvalidData.into());
        }
        bytes.extend_from_slice(&chunk[..length]);
        reader.consume(length);
        if end.is_some() {
            return String::from_utf8(bytes)
                .map(Some)
                .map_err(|_| io::ErrorKind::InvalidData.into());
        }
    }
}
pub fn send(value: &impl Serialize) -> io::Result<()> {
    let mut output = io::stdout().lock();
    serde_json::to_writer(&mut output, value)?;
    output.write_all(b"\n")?;
    output.flush()
}
pub fn valid_pages(pages: &[Page]) -> bool {
    pages.len() <= 8
        && pages.iter().all(|p| {
            p.id.len() <= 128
                && p.text.chars().count() <= 80
                && p.tooltip.chars().count() <= 1024
                && p.progress
                    .is_none_or(|n| n.is_finite() && (0.0..=1.0).contains(&n))
        })
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn bounded_input_and_unknown_quota() {
        assert!(read_line(&mut io::Cursor::new(vec![b'x'; MAX_LINE + 1])).is_err());
        let p = Page {
            text: "Claude —".into(),
            ..Page::default()
        };
        assert!(valid_pages(&[p]));
        assert!(!valid_pages(&[Page {
            progress: Some(f64::NAN),
            ..Page::default()
        }]));
    }
}
