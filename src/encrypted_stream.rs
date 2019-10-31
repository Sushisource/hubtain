use anyhow::{anyhow, Error};
use futures::{task::Context, AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use ring::aead::{Aad, LessSafeKey, Nonce, UnboundKey, CHACHA20_POLY1305};
use serde::{Deserialize, Serialize};
use std::cmp::min;
use std::io::{Cursor, Write};
use std::{io, pin::Pin, task::Poll};
use x25519_dalek::{EphemeralSecret, PublicKey, SharedSecret};

pub struct EncryptedStreamStarter<'a, S: AsyncWrite + AsyncRead> {
    underlying: Pin<&'a mut S>,
    secret: EphemeralSecret,
}

#[derive(Serialize, Deserialize, Debug)]
struct Handshake {
    pkey: [u8; 32],
}

impl<'a, S> EncryptedStreamStarter<'a, S>
where
    S: AsyncWrite + AsyncRead + Unpin,
{
    pub fn new(underlying_stream: &'a mut S, secret: EphemeralSecret) -> Self {
        EncryptedStreamStarter {
            underlying: Pin::new(underlying_stream),
            secret,
        }
    }

    /// Performs DH key exchange with the other side of the stream. Returns a new version
    /// of the stream that can be read/write from, transparently encrypting/decrypting the
    /// data.
    pub async fn key_exchange(mut self) -> Result<EncryptedStream<'a, S>, Error> {
        let pubkey = PublicKey::from(&self.secret);
        let outgoing_hs = Handshake {
            pkey: *pubkey.as_bytes(),
        };
        // Exchange public keys, first send ours
        let send_hs = bincode::serialize(&outgoing_hs)?;
        self.underlying.write_all(send_hs.as_slice()).await?;

        // Read incoming pubkey
        let read_size = bincode::serialized_size(&outgoing_hs)?;
        let mut buff = vec![0; read_size as usize];
        self.underlying.read_exact(&mut buff).await?;
        let read_hs: Handshake = bincode::deserialize_from(buff.as_slice())?;

        // Compute shared secret
        let shared_secret = self.secret.diffie_hellman(&read_hs.pkey.into());

        // TODO: Agree on nonce sequence here?

        EncryptedStream::new(self.underlying, shared_secret)
    }
}

pub struct EncryptedStream<'a, S: AsyncWrite + AsyncRead> {
    underlying: Pin<&'a mut S>,
    key: LessSafeKey,
    read_remainder: Vec<u8>,
}

impl<'a, S> EncryptedStream<'a, S>
where
    S: AsyncWrite + AsyncRead,
{
    fn new(underlying: Pin<&'a mut S>, shared_secret: SharedSecret) -> Result<Self, Error> {
        let unbound_k = UnboundKey::new(&CHACHA20_POLY1305, shared_secret.as_bytes())
            .map_err(|_| anyhow!("Couldn't bind key"))?;
        // key used to encrypt data
        let key = LessSafeKey::new(unbound_k);

        Ok(Self {
            underlying,
            key,
            read_remainder: vec![],
        })
    }
}

const CHUNK_SIZE: usize = 5012;
#[derive(Serialize, Deserialize)]
struct EncryptedPacket {
    data: Vec<u8>,
}

impl<'a, S> AsyncWrite for EncryptedStream<'a, S>
where
    S: AsyncWrite + AsyncRead,
{
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context,
        buf: &[u8],
    ) -> Poll<Result<usize, io::Error>> {
        // TODO: have both sides agree on nonce sequence somehow?
        let nonce = Nonce::assume_unique_for_key([0; 12]);

        let siz = min(buf.len(), CHUNK_SIZE);
        let mut encrypt_me = vec![0; siz];
        encrypt_me.copy_from_slice(&buf[0..siz]);
        let bufsiz = encrypt_me.len();
        //        dbg!(&bufsiz);
        self.key
            .seal_in_place_append_tag(nonce, Aad::empty(), &mut encrypt_me)
            .unwrap();

        //        dbg!(encrypt_me.len());
        let packet = EncryptedPacket { data: encrypt_me };
        let bincoded = bincode::serialize(&packet).unwrap();
        self.underlying
            .as_mut()
            .poll_write(cx, bincoded.as_slice())
            .map(|r| match r {
                Ok(really_wrote) => {
                    //                    dbg!(really_wrote);
                    Ok(bufsiz)
                }
                o => o,
            })
    }

    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context) -> Poll<Result<(), io::Error>> {
        self.underlying.as_mut().poll_flush(cx)
    }

    fn poll_close(mut self: Pin<&mut Self>, cx: &mut Context) -> Poll<Result<(), io::Error>> {
        self.underlying.as_mut().poll_close(cx)
    }
}

impl<'a, S> AsyncRead for EncryptedStream<'a, S>
where
    S: AsyncRead + AsyncWrite,
{
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context,
        buf: &mut [u8],
    ) -> Poll<Result<usize, io::Error>> {
        // TODO: have both sides agree on nonce sequence somehow?
        let nonce = Nonce::assume_unique_for_key([0; 12]);

        // TODO: How to ensure sufficient padding here...?
        let mut read_buf = vec![0; CHUNK_SIZE * 2];
        let read = self.underlying.as_mut().poll_read(cx, &mut read_buf);

        match &read {
            Poll::Ready(Ok(bytes_read)) => {
                if *bytes_read == 0 {
                    if self.read_remainder.is_empty() {
                        return read;
                    }
                    let mut cursord = Cursor::new(self.read_remainder.as_slice());
                    // TODO: so much dupe if this is the real fix
                    let packet: EncryptedPacket = match bincode::deserialize_from(&mut cursord) {
                        Ok(p) => p,
                        Err(e) => return Poll::Pending,
                    };
                    let cpos = cursord.position();
                    self.read_remainder = self.read_remainder.split_off(cpos as usize);
                    let mut decrypt_buff = packet.data;
                    dbg!(&decrypt_buff.len());

                    if decrypt_buff.is_empty() {
                        return Poll::Ready(Ok(0));
                    }

                    let just_content = self
                        .key
                        .open_in_place(nonce, Aad::empty(), &mut decrypt_buff)
                        .unwrap();
                    let mut cursor = Cursor::new(buf);
                    Write::write_all(&mut cursor, &just_content).expect("Internal write");
                    dbg!(just_content.len());
                    return Poll::Ready(Ok(just_content.len()));
                }
                dbg!(&bytes_read);
                dbg!(self.read_remainder.len());
                let remainder_len = self.read_remainder.len();
                let mut remainder_plus_read =
                    [self.read_remainder.as_slice(), read_buf.as_slice()].concat();
                let mut cursord = Cursor::new(&remainder_plus_read);
                // If for whatever reason we read only a partial packet, or we read a packet
                // plus some extra last go-round, we can get an unexpected eof error deserializing,
                // so (TODO: ACTUALLY DETECT IT) we detect that and continue
                // TODO: IDEA -- use AsyncBufRead for this? Since I am now in fact buffering?
                //   it seems bad/verboten to not have the bytes read from the source == bytes
                //   copied into buf
                let deser_res = bincode::deserialize_from(&mut cursord);
                let cursor_pos = cursord.position();
                // Drop portion of the buffer which is just useless zero padding, if any.
                let useless_buffer_bytes = read_buf.len() - *bytes_read;
                remainder_plus_read.split_off(remainder_plus_read.len() - useless_buffer_bytes);

                dbg!(&cursor_pos);
                let packet: EncryptedPacket = match deser_res {
                    Ok(p) => {
                        // Drop however much of the buffer we *did* read
                        self.read_remainder = remainder_plus_read.split_off(cursor_pos as usize);
                        p
                    }
                    Err(e) => {
                        // TODO: Match unexpected EOF
                        dbg!(e);
                        // If we did not read a packet succesfully, store the portion we received
                        // in the remainder
                        let remaining = self.read_remainder.split_off(cursor_pos as usize);
                        dbg!(remaining.len());
                        dbg!(self.read_remainder.len());
                        if remaining.len() == 0 {
                            dbg!("No extra");
                            //                            cx.waker().wake_by_ref();
                            //                            return Poll::Pending;
                        }
                        return Poll::Ready(Ok(self.read_remainder.len()));
                    }
                };
                let mut decrypt_buff = packet.data;
                dbg!(&decrypt_buff.len());
                if decrypt_buff.is_empty() {
                    return Poll::Ready(Ok(*bytes_read));
                }

                let just_content = self
                    .key
                    .open_in_place(nonce, Aad::empty(), &mut decrypt_buff)
                    .unwrap();
                let mut cursor = Cursor::new(buf);
                Write::write_all(&mut cursor, &just_content).expect("Internal write");
                dbg!(just_content.len());
                //                return Poll::Ready(Ok(*bytes_read));
                return Poll::Ready(Ok(just_content.len()));
            }
            _ => (),
        };
        read
    }
}
