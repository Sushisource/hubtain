use serde::{export::Formatter, Deserialize, Serialize};
use std::{
    collections::hash_map::DefaultHasher,
    fmt::{self, Debug, Display},
    hash::{Hash, Hasher},
};

#[derive(Serialize, Deserialize)]
pub struct HandshakeReply {
    pub server_name: String,
    pub tcp_port: u16,
    pub data_length: u64,
    pub encrypted: bool,
    pub file_name: String,
}

#[derive(Copy, Clone, Constructor, Hash, Eq, PartialEq)]
pub struct ClientId {
    pubkey: [u8; 32],
}

impl Display for ClientId {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        let mut s = DefaultHasher::new();
        self.pubkey.hash(&mut s);
        let hashed_key = s.finish();
        write!(f, "{}", mnemonic::to_string(hashed_key.to_le_bytes()))
    }
}

impl Debug for ClientId {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        Display::fmt(self, f)
    }
}
