//! Copying text to the system clipboard.

use std::io::Write;

/// Copy text to the clipboard. Uses the system clipboard, and falls back to
/// the OSC 52 terminal escape code (works over SSH and in tmux with
/// `set -g set-clipboard on`) when there is no system clipboard.
pub fn copy(text: &str) -> anyhow::Result<()> {
    let system = arboard::Clipboard::new().and_then(|mut clipboard| clipboard.set_text(text));
    if system.is_ok() {
        return Ok(());
    }
    let mut stdout = std::io::stdout();
    write!(stdout, "\x1b]52;c;{}\x07", base64(text.as_bytes()))?;
    stdout.flush()?;
    Ok(())
}

fn base64(bytes: &[u8]) -> String {
    const ALPHABET: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let n = chunk.iter().enumerate().fold(0u32, |n, (i, b)| n | (*b as u32) << (16 - 8 * i));
        for i in 0..4 {
            if i <= chunk.len() {
                out.push(ALPHABET[(n >> (18 - 6 * i) & 63) as usize] as char);
            } else {
                out.push('=');
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_base64() {
        assert_eq!(base64(b""), "");
        assert_eq!(base64(b"f"), "Zg==");
        assert_eq!(base64(b"fo"), "Zm8=");
        assert_eq!(base64(b"foo"), "Zm9v");
        assert_eq!(base64(b"claude --resume"), "Y2xhdWRlIC0tcmVzdW1l");
    }
}
