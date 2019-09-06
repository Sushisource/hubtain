use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize)]
pub struct HandshakeReply {
    pub server_name: String,
    pub tcp_port: u16,
    pub data_length: u64,
}
