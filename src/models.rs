use serde::{export::Formatter, Deserialize, Serialize};
use std::fmt::{self, Debug, Display};

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
        write!(f, "{}", mnemonic::to_string(self.pubkey))
    }
}

impl Debug for ClientId {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        Display::fmt(self, f)
    }
}
