//! SSH 算法档位。
//!
//! S1 要回答的问题不是"russh 能不能连老服务器"，而是：
//! **默认配置连不上时，能不能通过改配置连上？**
//! 所以这里定义两组：服务端模拟档位（现代 / 全老 / 只给 ssh-rsa）
//! 与客户端档位（默认 / 放宽）。

use std::borrow::Cow;

use russh::keys::Algorithm;
use russh::{cipher, kex, mac, Preferred};

fn kex_list(names: &[&str]) -> Vec<kex::Name> {
    names
        .iter()
        .map(|n| kex::Name::try_from(*n).unwrap_or_else(|_| panic!("russh 不认识这个 kex: {n}")))
        .collect()
}

fn cipher_list(names: &[&str]) -> Vec<cipher::Name> {
    names
        .iter()
        .map(|n| {
            cipher::Name::try_from(*n).unwrap_or_else(|_| panic!("russh 不认识这个 cipher: {n}"))
        })
        .collect()
}

fn mac_list(names: &[&str]) -> Vec<mac::Name> {
    names
        .iter()
        .map(|n| mac::Name::try_from(*n).unwrap_or_else(|_| panic!("russh 不认识这个 mac: {n}")))
        .collect()
}

fn key_list(names: &[&str]) -> Vec<Algorithm> {
    names
        .iter()
        .map(|n| {
            n.parse::<Algorithm>()
                .unwrap_or_else(|_| panic!("russh 不认识这个 host key 算法: {n}"))
        })
        .collect()
}

/// 现代服务器：贴近 OpenSSH 9.x/10.x 默认行为。
pub fn modern() -> Preferred {
    Preferred {
        kex: Cow::Owned(kex_list(&[
            "curve25519-sha256",
            "curve25519-sha256@libssh.org",
            "diffie-hellman-group16-sha512",
            "diffie-hellman-group14-sha256",
        ])),
        key: Cow::Owned(key_list(&["ssh-ed25519", "rsa-sha2-256", "rsa-sha2-512"])),
        cipher: Cow::Owned(cipher_list(&[
            "chacha20-poly1305@openssh.com",
            "aes256-gcm@openssh.com",
            "aes256-ctr",
        ])),
        mac: Cow::Owned(mac_list(&[
            "hmac-sha2-256-etm@openssh.com",
            "hmac-sha2-256",
        ])),
        ..Preferred::DEFAULT
    }
}

/// 老服务器：模拟 CentOS 6 / 强制旧算法的 CentOS 7，只有 SHA-1 那一套。
pub fn legacy() -> Preferred {
    Preferred {
        kex: Cow::Owned(kex_list(&[
            "diffie-hellman-group14-sha1",
            "diffie-hellman-group1-sha1",
        ])),
        key: Cow::Owned(key_list(&["ssh-rsa"])),
        cipher: Cow::Owned(cipher_list(&["aes128-cbc", "aes256-cbc"])),
        mac: Cow::Owned(mac_list(&["hmac-sha1"])),
        ..Preferred::DEFAULT
    }
}

/// 只给 ssh-rsa 主机密钥、其余现代的服务器（很常见的"加固不彻底"状态）。
pub fn rsa_only() -> Preferred {
    Preferred {
        kex: Cow::Owned(kex_list(&[
            "curve25519-sha256",
            "diffie-hellman-group14-sha256",
        ])),
        key: Cow::Owned(key_list(&["ssh-rsa"])),
        cipher: Cow::Owned(cipher_list(&["aes256-ctr", "aes128-ctr"])),
        mac: Cow::Owned(mac_list(&["hmac-sha2-256"])),
        ..Preferred::DEFAULT
    }
}

/// 客户端默认档位：直接吃 russh 的 DEFAULT。
pub fn client_default() -> Preferred {
    Preferred::DEFAULT
}

/// 客户端的"放宽"档位：现代算法仍然排前面，只在后面追加老算法兜底。
pub fn client_relaxed() -> Preferred {
    let mut kex = Preferred::DEFAULT.kex.to_vec();
    kex.extend(kex_list(&[
        "diffie-hellman-group14-sha1",
        "diffie-hellman-group-exchange-sha1",
        "diffie-hellman-group1-sha1",
    ]));

    let mut keys = Preferred::DEFAULT.key.to_vec();
    for algo in key_list(&["ssh-rsa"]) {
        if !keys.contains(&algo) {
            keys.push(algo);
        }
    }

    let mut ciphers = Preferred::DEFAULT.cipher.to_vec();
    ciphers.extend(cipher_list(&["aes128-cbc", "aes192-cbc", "aes256-cbc"]));

    let mut macs = Preferred::DEFAULT.mac.to_vec();
    macs.extend(mac_list(&["hmac-sha1", "hmac-sha1-etm@openssh.com"]));

    Preferred {
        kex: Cow::Owned(kex),
        key: Cow::Owned(keys),
        cipher: Cow::Owned(ciphers),
        mac: Cow::Owned(macs),
        ..Preferred::DEFAULT
    }
}

/// 把档位里声明的算法打印成一行，方便人工核对。
pub fn describe(pref: &Preferred) -> String {
    format!(
        "kex={:?} key={:?} cipher={:?} mac={:?}",
        pref.kex.iter().map(|n| n.as_ref()).collect::<Vec<_>>(),
        pref.key.iter().map(|a| a.to_string()).collect::<Vec<_>>(),
        pref.cipher.iter().map(|n| n.as_ref()).collect::<Vec<_>>(),
        pref.mac.iter().map(|n| n.as_ref()).collect::<Vec<_>>(),
    )
}

/// 服务端主机密钥算法：现代用 ed25519，老档位用 RSA（因为老档位只认 ssh-rsa）。
pub fn host_key_algorithm(for_profile: &str) -> Algorithm {
    match for_profile {
        "modern" => Algorithm::Ed25519,
        _ => Algorithm::Rsa { hash: None },
    }
}

/// 只用于文档：列出 russh 0.65 实际实现但默认不启用的算法。
pub const IMPLEMENTED_BUT_NOT_DEFAULT: &[&str] = &[
    "kex: diffie-hellman-group14-sha1 / group1-sha1 / group-exchange-sha1",
    "cipher: aes128-cbc / aes192-cbc / aes256-cbc",
    "mac: hmac-sha1 / hmac-sha1-etm@openssh.com",
    "host key: ssh-rsa 已在默认 key 列表里（Algorithm::Rsa { hash: None }）",
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn all_profiles_build() {
        for p in [
            modern(),
            legacy(),
            rsa_only(),
            client_default(),
            client_relaxed(),
        ] {
            assert!(!p.kex.is_empty());
            assert!(!p.key.is_empty());
            assert!(!p.cipher.is_empty());
        }
    }

    #[test]
    fn relaxed_profile_keeps_modern_algorithms_first() {
        let relaxed = client_relaxed();
        let default = client_default();
        assert_eq!(relaxed.kex[0], default.kex[0], "放宽不能让老算法抢到最前面");
        assert!(relaxed.kex.iter().any(|n| *n == kex::DH_G14_SHA1));
    }

    #[test]
    fn default_profile_lacks_sha1_kex() {
        let default = client_default();
        assert!(!default.kex.iter().any(|n| *n == kex::DH_G14_SHA1));
    }
}
