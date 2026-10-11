//! 本机文件摘要计算：有界内存逐块读取，不向对端或 SDK 发送内容。

use std::io::{self, Read};

use md5::Md5;
use sha2::{Digest, Sha256};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum HashAlgorithm {
    Sha256,
    Md5,
}

impl HashAlgorithm {
    pub fn name(self) -> &'static str {
        match self {
            Self::Sha256 => "SHA-256",
            Self::Md5 => "MD5",
        }
    }
}

/// 通过已经授权打开的文件句柄计算摘要，避免重新打开文件造成路径竞争。
pub fn hash_reader(mut reader: impl Read, algorithm: HashAlgorithm) -> io::Result<String> {
    enum Hasher {
        Sha256(Sha256),
        Md5(Md5),
    }
    let mut hasher = match algorithm {
        HashAlgorithm::Sha256 => Hasher::Sha256(Sha256::new()),
        HashAlgorithm::Md5 => Hasher::Md5(Md5::new()),
    };
    let mut buffer = [0u8; 128 * 1024];
    loop {
        let size = reader.read(&mut buffer)?;
        if size == 0 { break; }
        match &mut hasher {
            Hasher::Sha256(state) => state.update(&buffer[..size]),
            Hasher::Md5(state) => state.update(&buffer[..size]),
        }
    }
    Ok(match hasher {
        Hasher::Sha256(state) => format!("{:x}", state.finalize()),
        Hasher::Md5(state) => format!("{:x}", state.finalize()),
    })
}

    
/// 一次读取文件同时计算两个摘要，适合大文件快速交叉比对。
pub fn hash_reader_both(mut reader: impl Read) -> io::Result<(String, String)> {
    let mut sha256 = Sha256::new();
    let mut md5 = Md5::new();
    let mut buffer = [0u8; 128 * 1024];
    loop {
        let size = reader.read(&mut buffer)?;
        if size == 0 { break; }
        sha256.update(&buffer[..size]);
        md5.update(&buffer[..size]);
    }
    Ok((format!("{:x}", sha256.finalize()), format!("{:x}", md5.finalize())))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn known_hash_vectors_for_empty_and_abc() {
        let examples = [
            ("", "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855", "d41d8cd98f00b204e9800998ecf8427e"),
            ("abc", "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad", "900150983cd24fb0d6963f7d28e17f72"),
        ];
        for (value, sha, md5) in examples {
            assert_eq!(hash_reader(value.as_bytes(), HashAlgorithm::Sha256).unwrap(), sha);
            assert_eq!(hash_reader(value.as_bytes(), HashAlgorithm::Md5).unwrap(), md5);
        }
    }

    #[test]
    fn simultaneous_hashes_match_standard_vectors() {
        for (value, sha, md5) in [
            ("", "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855", "d41d8cd98f00b204e9800998ecf8427e"),
            ("abc", "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad", "900150983cd24fb0d6963f7d28e17f72"),
        ] {
            let (actual_sha, actual_md5) = hash_reader_both(value.as_bytes()).unwrap();
            assert_eq!(actual_sha, sha);
            assert_eq!(actual_md5, md5);
        }
    }

    #[test]
    fn simultaneous_hashes_propagate_io_errors() {
        struct Faulty;
        impl Read for Faulty {
            fn read(&mut self, _: &mut [u8]) -> io::Result<usize> {
                Err(io::Error::other("read failed"))
            }
        }
        assert!(hash_reader_both(Faulty).is_err());
    }

    #[test]
    fn large_input_is_processed_incrementally() {
        let data = vec![b'a'; 1_000_000];
        assert_eq!(hash_reader(data.as_slice(), HashAlgorithm::Sha256).unwrap(),
            "cdc76e5c9914fb9281a1c7e284d73e67f1809a48a497200e046d39ccc7112cd0");
        assert_eq!(hash_reader(data.as_slice(), HashAlgorithm::Md5).unwrap(),
            "7707d6ae4e027c70eea2a935c2296f21");
    }

    #[test]
    fn propagates_reader_errors() {
        struct Broken;
        impl Read for Broken {
            fn read(&mut self, _buf: &mut [u8]) -> io::Result<usize> {
                Err(io::Error::other("read failed"))
            }
        }
        assert!(hash_reader(Broken, HashAlgorithm::Sha256).is_err());
        assert!(hash_reader(Broken, HashAlgorithm::Md5).is_err());
    }
}
