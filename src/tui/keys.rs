//! Encode key events as the bytes a terminal program expects.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

pub fn encode(key: &KeyEvent, app_cursor: bool) -> Option<Vec<u8>> {
    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
    let alt = key.modifiers.contains(KeyModifiers::ALT);
    let mut out = match key.code {
        KeyCode::Char(c) if ctrl => {
            let b = match c.to_ascii_lowercase() {
                c @ 'a'..='z' => c as u8 - b'a' + 1,
                ' ' | '@' | '2' => 0,
                '[' | '3' => 27,
                '\\' | '4' => 28,
                ']' | '5' => 29,
                '^' | '6' => 30,
                '_' | '7' | '/' => 31,
                '8' => 127,
                _ => return None,
            };
            vec![b]
        }
        KeyCode::Char(c) => c.to_string().into_bytes(),
        KeyCode::Enter => vec![b'\r'],
        KeyCode::Tab => vec![b'\t'],
        KeyCode::BackTab => b"\x1b[Z".to_vec(),
        KeyCode::Backspace => vec![0x7f],
        KeyCode::Esc => vec![0x1b],
        KeyCode::Up => cursor(b'A', app_cursor),
        KeyCode::Down => cursor(b'B', app_cursor),
        KeyCode::Right => cursor(b'C', app_cursor),
        KeyCode::Left => cursor(b'D', app_cursor),
        KeyCode::Home => cursor(b'H', app_cursor),
        KeyCode::End => cursor(b'F', app_cursor),
        KeyCode::PageUp => b"\x1b[5~".to_vec(),
        KeyCode::PageDown => b"\x1b[6~".to_vec(),
        KeyCode::Delete => b"\x1b[3~".to_vec(),
        KeyCode::Insert => b"\x1b[2~".to_vec(),
        KeyCode::F(n) => match n {
            1 => b"\x1bOP".to_vec(),
            2 => b"\x1bOQ".to_vec(),
            3 => b"\x1bOR".to_vec(),
            4 => b"\x1bOS".to_vec(),
            5 => b"\x1b[15~".to_vec(),
            6 => b"\x1b[17~".to_vec(),
            7 => b"\x1b[18~".to_vec(),
            8 => b"\x1b[19~".to_vec(),
            9 => b"\x1b[20~".to_vec(),
            10 => b"\x1b[21~".to_vec(),
            11 => b"\x1b[23~".to_vec(),
            12 => b"\x1b[24~".to_vec(),
            _ => return None,
        },
        _ => return None,
    };
    if alt {
        out.insert(0, 0x1b);
    }
    Some(out)
}

fn cursor(c: u8, app: bool) -> Vec<u8> {
    if app {
        vec![0x1b, b'O', c]
    } else {
        vec![0x1b, b'[', c]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn basics() {
        let k = |code, m| KeyEvent::new(code, m);
        assert_eq!(encode(&k(KeyCode::Char('c'), KeyModifiers::CONTROL), false), Some(vec![3]));
        assert_eq!(encode(&k(KeyCode::Char('x'), KeyModifiers::ALT), false), Some(vec![0x1b, b'x']));
        assert_eq!(encode(&k(KeyCode::Up, KeyModifiers::NONE), true), Some(b"\x1bOA".to_vec()));
        assert_eq!(encode(&k(KeyCode::Enter, KeyModifiers::NONE), false), Some(vec![b'\r']));
    }
}
