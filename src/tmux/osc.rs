//! Bounded OSC observer; raw pane output is never removed or rewritten.
const LIMIT: usize = 64 * 1024;
#[derive(Default)]
pub struct Osc7501 {
    state: State,
}
#[derive(Default)]
enum State {
    #[default]
    Ground,
    Esc,
    Osc {
        bytes: Vec<u8>,
        esc: bool,
        overflow: bool,
    },
}
impl Osc7501 {
    pub fn reset(&mut self) {
        self.state = State::Ground;
    }
    pub fn feed(&mut self, input: &[u8]) -> Vec<Vec<u8>> {
        let mut out = Vec::new();
        for &b in input {
            match &mut self.state {
                State::Ground => {
                    if b == 27 {
                        self.state = State::Esc;
                    }
                }
                State::Esc => {
                    self.state = if b == b']' {
                        State::Osc { bytes: Vec::new(), esc: false, overflow: false }
                    } else if b == 27 {
                        State::Esc
                    } else {
                        State::Ground
                    }
                }
                State::Osc { bytes, esc, overflow } => {
                    if b == 7 || (*esc && b == b'\\') {
                        if !*overflow && bytes.starts_with(b"7501;") {
                            out.push(bytes[5..].to_vec());
                        }
                        self.state = State::Ground;
                    } else if b == 0x18 || b == 0x1a {
                        self.state = State::Ground;
                    } else if b == 27 {
                        *esc = true;
                    } else if *esc {
                        // ESC not followed by ST cancels this OSC, rather than joining unrelated data.
                        self.state = if b == b']' { State::Osc { bytes: Vec::new(), esc: false, overflow: false } } else { State::Ground };
                    } else if bytes.len() < LIMIT {
                        bytes.push(b);
                    } else {
                        *overflow = true;
                    }
                }
            }
        }
        out
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn split_terminators_and_independent_panes() {
        let mut a = Osc7501::default();
        let mut b = Osc7501::default();
        assert!(a.feed(b"before\x1b]7501;{\"state\":\"work").is_empty());
        assert_eq!(b.feed(b"\x1b]7501;beta\x1b\\"), vec![b"beta".to_vec()]);
        assert!(a.feed(b"ing\"}\x1b").is_empty());
        assert_eq!(a.feed(b"\\after"), vec![b"{\"state\":\"working\"}".to_vec()]);
        assert_eq!(a.feed(b"\x1b]0;title\x07\x1b]7501;done\x07"), vec![b"done".to_vec()]);
    }
    #[test]
    fn gap_and_oversize_never_fabricate_status() {
        let mut p = Osc7501::default();
        p.feed(b"\x1b]7501;partial");
        p.reset();
        assert!(p.feed(b"tail\x07").is_empty());
        p.feed(b"\x1b]7501;");
        p.feed(&vec![b'x'; LIMIT + 1]);
        assert!(p.feed(b"\x07").is_empty());
        assert_eq!(p.feed(b"\x1b]7501;ok\x07"), vec![b"ok".to_vec()]);
    }
}
