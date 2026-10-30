//! 客户端算法档位。
//!
//! 依据 S1 Spike 结论（docs/spike-notes.md §1）：
//! russh 其实实现了 `diffie-hellman-group14-sha1` / `group1-sha1` / `aes*-cbc` / `hmac-sha1`，
//! 只是默认的 `Preferred` 不含它们。所以「兼容老服务器」不需要换库，只需要按需追加。

use std::borrow::Cow;

use russh::{cipher, kex, mac, Preferred};

fn kex_from(names: &[&str]) -> Vec<kex::Name> {
    // 认不出来就跳过，不要 panic —— 算法名是编译期常量，没必要为一个笔误炸掉整个连接
    names
        .iter()
        .filter_map(|n| kex::Name::try_from(*n).ok())
        .collect()
}

fn cipher_from(names: &[&str]) -> Vec<cipher::Name> {
    names
        .iter()
        .filter_map(|n| cipher::Name::try_from(*n).ok())
        .collect()
}

fn mac_from(names: &[&str]) -> Vec<mac::Name> {
    names
        .iter()
        .filter_map(|n| mac::Name::try_from(*n).ok())
        .collect()
}

/// 默认档位：完全用 russh 的默认列表（现代算法）。
pub fn modern() -> Preferred {
    Preferred::DEFAULT
}

/// 兼容档位：现代算法**仍然排在最前**，只在后面追加老算法兜底。
///
/// 顺序不能反 —— 一旦老算法被排到前面，连现代服务器时也会退化成 SHA-1，
/// 那就等于为了兼容性把安全性无差别降级了。
pub fn legacy() -> Preferred {
    let mut kex_list = Preferred::DEFAULT.kex.to_vec();
    kex_list.extend(kex_from(&[
        "diffie-hellman-group14-sha1",
        "diffie-hellman-group-exchange-sha1",
        "diffie-hellman-group1-sha1",
    ]));

    let mut cipher_list = Preferred::DEFAULT.cipher.to_vec();
    cipher_list.extend(cipher_from(&["aes128-cbc", "aes192-cbc", "aes256-cbc"]));

    let mut mac_list = Preferred::DEFAULT.mac.to_vec();
    mac_list.extend(mac_from(&["hmac-sha1", "hmac-sha1-etm@openssh.com"]));

    Preferred {
        kex: Cow::Owned(kex_list),
        cipher: Cow::Owned(cipher_list),
        mac: Cow::Owned(mac_list),
        ..Preferred::DEFAULT
    }
}

pub fn for_connection(legacy_algorithms: bool) -> Preferred {
    if legacy_algorithms {
        legacy()
    } else {
        modern()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn modern_profile_matches_default() {
        assert_eq!(modern().kex.len(), Preferred::DEFAULT.kex.len());
        assert!(!modern().kex.contains(&kex::DH_G14_SHA1));
    }

    #[test]
    fn legacy_profile_appends_without_reordering() {
        let legacy = legacy();
        let default = Preferred::DEFAULT;

        assert_eq!(legacy.kex[0], default.kex[0], "老算法不能抢到最前面");
        assert!(legacy.kex.len() > default.kex.len());
        assert!(legacy.kex.contains(&kex::DH_G14_SHA1));
        assert!(legacy.cipher.iter().any(|n| n.as_ref() == "aes128-cbc"));
        assert!(legacy.mac.iter().any(|n| n.as_ref() == "hmac-sha1"));
    }

    #[test]
    fn for_connection_switches_on_flag() {
        assert_eq!(
            for_connection(false).kex.len(),
            Preferred::DEFAULT.kex.len()
        );
        assert!(for_connection(true).kex.len() > Preferred::DEFAULT.kex.len());
    }
}
