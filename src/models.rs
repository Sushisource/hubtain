use anyhow::Error;
use async_std::io::{Read, Write};
use bincode::{deserialize, serialize};
use futures::{AsyncReadExt, AsyncWriteExt};
use serde::{Deserialize, Serialize};
use std::{
    collections::hash_map::DefaultHasher,
    fmt::{self, Debug, Display},
    hash::{Hash, Hasher},
    fmt::Formatter
};

#[derive(Serialize, Deserialize, Debug, PartialEq)]
pub struct DataSrvInfo {
    pub tcp_port: u16,
    pub data_length: u64,
    pub encrypted: bool,
    pub data_name: String,
}

impl DataSrvInfo {
    // Having to implement these kinda sucks, but AFAICT there's not a great way to use serde
    // with async readers. I'm also certainly including an unnecessary length byte at the front.
    pub async fn write_to_stream<W: Write + Unpin>(&self, stream: &mut W) -> Result<(), Error> {
        let serialized = serialize(&self)?;
        let len_byte = [serialized.len() as u8];
        stream.write(&len_byte).await?;
        stream.write(&serialized).await?;
        Ok(())
    }

    pub async fn read_from_stream<R: Read + Unpin>(stream: &mut R) -> Result<Self, Error> {
        let mut len_byte = [0u8; 1];
        stream.read_exact(&mut len_byte).await?;
        let mut datbuf = vec![0u8; len_byte[0] as usize];
        stream.read_exact(&mut datbuf).await?;
        Ok(deserialize::<Self>(&datbuf)?)
    }
}

#[derive(Serialize, Deserialize)]
pub struct DiscoveryReply {
    pub server_name: String,
    pub tcp_port: u16,
}

#[derive(Clone, Constructor, Hash, Eq, PartialEq)]
pub struct ClientId {
    pubkey: Vec<u8>,
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
