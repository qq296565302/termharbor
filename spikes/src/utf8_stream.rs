//! UTF-8 增量解码器。
//!
//! 这是 S2 的核心结论之一：SSH 的分包边界会把一个多字节字符切成两半。
//! 直接用 `String::from_utf8_lossy(chunk)` 会在每个分包边界产生 `U+FFFD`（乱码），
//! 而这正是需求里"中文不乱码"那条验收项的杀手。
//!
//! 做法：把不完整的尾部字节缓存起来，等下一个分包补齐再一起解码。

/// 尾部最多缓存 3 字节（UTF-8 最长 4 字节，去掉至少 1 个已确认字节）。
const MAX_TAIL: usize = 3;

#[derive(Debug, Default)]
pub struct Utf8StreamDecoder {
    tail: Vec<u8>,
    /// 因非法字节而被替换掉的次数，用来判定"到底是分包问题还是数据本身有问题"。
    invalid_bytes: u64,
}

impl Utf8StreamDecoder {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn invalid_bytes(&self) -> u64 {
        self.invalid_bytes
    }

    /// 喂一段原始字节，返回可以安全交给终端的字符串。
    ///
    /// 不完整的尾部会被留在内部，下次调用时拼在前面。
    pub fn push(&mut self, chunk: &[u8]) -> String {
        let mut buf = std::mem::take(&mut self.tail);
        buf.extend_from_slice(chunk);

        let mut out = String::with_capacity(buf.len());
        let mut pos = 0;

        loop {
            match std::str::from_utf8(&buf[pos..]) {
                Ok(valid) => {
                    out.push_str(valid);
                    break;
                }
                Err(err) => {
                    let valid_up_to = err.valid_up_to();
                    // valid_up_to 之前的字节一定是合法 UTF-8
                    if let Ok(valid) = std::str::from_utf8(&buf[pos..pos + valid_up_to]) {
                        out.push_str(valid);
                    }
                    match err.error_len() {
                        // 真正的非法字节：用替换符顶掉，继续往后解
                        Some(len) => {
                            out.push('\u{FFFD}');
                            self.invalid_bytes += len as u64;
                            pos += valid_up_to + len;
                        }
                        // 结尾不完整：留下来等下一个分包
                        None => {
                            let rest = &buf[pos + valid_up_to..];
                            // 防御：真遇到垃圾数据也別让 tail 无限增长
                            if rest.len() <= MAX_TAIL {
                                self.tail = rest.to_vec();
                            } else {
                                out.push('\u{FFFD}');
                                self.invalid_bytes += rest.len() as u64;
                            }
                            break;
                        }
                    }
                }
            }
        }

        out
    }

    /// 流结束时调用，处理可能残留的不完整序列。
    pub fn finish(&mut self) -> String {
        if self.tail.is_empty() {
            return String::new();
        }
        self.invalid_bytes += self.tail.len() as u64;
        self.tail.clear();
        "\u{FFFD}".to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn naive(chunk: &[u8]) -> String {
        String::from_utf8_lossy(chunk).into_owned()
    }

    #[test]
    fn reassembles_cjk_split_at_every_boundary() {
        let text = "你好，世界！终端港 TermHarbor";
        let bytes = text.as_bytes();

        // 每一个可能的切分点都试一遍
        for split in 0..=bytes.len() {
            let mut decoder = Utf8StreamDecoder::new();
            let mut out = decoder.push(&bytes[..split]);
            out.push_str(&decoder.push(&bytes[split..]));
            out.push_str(&decoder.finish());
            assert_eq!(out, text, "在 {split} 处切分后解码结果不一致");
            assert_eq!(decoder.invalid_bytes(), 0, "在 {split} 处切分产生了替换符");
        }
    }

    #[test]
    fn byte_at_a_time_still_exact() {
        let text = "中文乱码是终端客户端的头号工单";
        let mut decoder = Utf8StreamDecoder::new();
        let mut out = String::new();
        for b in text.as_bytes() {
            out.push_str(&decoder.push(&[*b]));
        }
        out.push_str(&decoder.finish());
        assert_eq!(out, text);
        assert_eq!(decoder.invalid_bytes(), 0);
    }

    #[test]
    fn naive_lossy_actually_breaks_on_split() {
        // 反过来证明这件事值得做：朴素做法确实会坏
        let text = "你好";
        let bytes = text.as_bytes();
        let bad = format!("{}{}", naive(&bytes[..1]), naive(&bytes[1..]));
        assert_ne!(bad, text);
        assert!(bad.contains('\u{FFFD}'));
    }

    #[test]
    fn real_invalid_bytes_are_counted_not_buffered() {
        let mut decoder = Utf8StreamDecoder::new();
        let out = decoder.push(&[0x41, 0xFF, 0x42]);
        assert!(out.starts_with('A'));
        assert!(out.contains('\u{FFFD}'));
        assert!(out.ends_with('B'));
        assert!(decoder.invalid_bytes() >= 1);
        assert!(decoder.tail.is_empty());
    }

    #[test]
    fn ansi_escape_sequences_pass_through_untouched() {
        let mut decoder = Utf8StreamDecoder::new();
        let s = "\x1b[1;32mOK\x1b[0m\r\n";
        assert_eq!(decoder.push(s.as_bytes()), s);
    }
}
