//! Encrypted, token-authenticated transport for every Archon link.
//!
//! All connections (client↔control plane and control plane↔agent) run a
//! Noise `XXpsk3` handshake over TCP before any frames flow. The shared
//! token is the pre-shared key: it never crosses the wire, possession is
//! proven cryptographically on both sides, and session keys have forward
//! secrecy. An empty token derives a public PSK — links stay encrypted but
//! unauthenticated; that degraded mode is the documented open mode.
//!
//! Framing above this layer is unchanged: `SecureStream` implements
//! `Read`/`Write` so the existing length-prefixed JSON frame code works
//! as-is. Ciphertext itself travels in u16 big-endian length-prefixed
//! Noise messages.

use std::io::{self, Read, Write};
use std::net::TcpStream;

const PATTERN: &str = "Noise_XXpsk3_25519_ChaChaPoly_SHA256";
/// Noise AEAD tag size.
const TAG_LEN: usize = 16;
/// Largest plaintext one Noise message can carry (MAXMSGLEN minus tag).
const MAX_PLAINTEXT: usize = 65535 - TAG_LEN;

fn psk_for(token: Option<&str>) -> [u8; 32] {
    use sha2::{Digest, Sha256};
    let mut hash = Sha256::new();
    hash.update(b"archon-noise:");
    hash.update(token.unwrap_or("").as_bytes());
    hash.finalize().into()
}

fn new_handshake(psk: &[u8; 32], initiator: bool) -> io::Result<snow::HandshakeState> {
    let params: snow::params::NoiseParams = PATTERN.parse().expect("valid protocol name");
    // The PSK carries all authentication; the pattern still transmits a
    // local static key, so each connection gets a fresh throwaway one.
    let keypair = snow::Builder::new(params.clone())
        .generate_keypair()
        .map_err(io::Error::other)?;
    let builder = snow::Builder::new(params)
        .local_private_key(&keypair.private)
        .map_err(io::Error::other)?
        // XXpsk3's token carries its literal suffix; supply at index 3.
        .psk(3, psk)
        .map_err(io::Error::other)?;
    let state = if initiator {
        builder.build_initiator()
    } else {
        builder.build_responder()
    };
    state.map_err(io::Error::other)
}

fn read_raw_frame(stream: &mut TcpStream) -> io::Result<Vec<u8>> {
    let mut len = [0u8; 2];
    stream.read_exact(&mut len)?;
    let mut payload = vec![0u8; u16::from_be_bytes(len) as usize];
    stream.read_exact(&mut payload)?;
    Ok(payload)
}

fn write_raw_frame(stream: &mut TcpStream, payload: &[u8]) -> io::Result<()> {
    if payload.len() > u16::MAX as usize {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "handshake message too large",
        ));
    }
    stream.write_all(&(payload.len() as u16).to_be_bytes())?;
    stream.write_all(payload)
}

/// Complete the Noise XXpsk3 handshake as the dialing side.
pub fn establish_initiator(mut stream: TcpStream, token: Option<&str>) -> io::Result<SecureStream> {
    let mut hs = new_handshake(&psk_for(token), true)?;
    let mut buf = vec![0u8; MAX_PLAINTEXT + TAG_LEN];
    let n = hs.write_message(&[], &mut buf).map_err(io::Error::other)?;
    write_raw_frame(&mut stream, &buf[..n])?;

    let msg2 = read_raw_frame(&mut stream)?;
    let mut out = vec![0u8; MAX_PLAINTEXT];
    let _ = hs.read_message(&msg2, &mut out).map_err(io::Error::other)?;

    let n = hs.write_message(&[], &mut buf).map_err(io::Error::other)?;
    write_raw_frame(&mut stream, &buf[..n])?;

    let transport = hs.into_transport_mode().map_err(io::Error::other)?;
    Ok(SecureStream {
        stream,
        noise: transport,
        pending: Vec::new(),
    })
}

/// Complete the Noise XXpsk3 handshake as the listening side.
pub fn establish_responder(mut stream: TcpStream, token: Option<&str>) -> io::Result<SecureStream> {
    let mut hs = new_handshake(&psk_for(token), false)?;
    let msg1 = read_raw_frame(&mut stream)?;
    let mut out = vec![0u8; MAX_PLAINTEXT];
    let _ = hs.read_message(&msg1, &mut out).map_err(io::Error::other)?;

    let mut buf = vec![0u8; MAX_PLAINTEXT + TAG_LEN];
    let n = hs.write_message(&[], &mut buf).map_err(io::Error::other)?;
    write_raw_frame(&mut stream, &buf[..n])?;

    let msg3 = read_raw_frame(&mut stream)?;
    let _ = hs.read_message(&msg3, &mut out).map_err(io::Error::other)?;

    let transport = hs.into_transport_mode().map_err(io::Error::other)?;
    Ok(SecureStream {
        stream,
        noise: transport,
        pending: Vec::new(),
    })
}

/// A TCP connection whose traffic is encrypted under the Noise session.
pub struct SecureStream {
    stream: TcpStream,
    noise: snow::TransportState,
    /// Plaintext decrypted from the last ciphertext frame, not yet
    /// handed to a caller of [`Read::read`].
    pending: Vec<u8>,
}

impl Read for SecureStream {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if buf.is_empty() {
            return Ok(0);
        }
        while self.pending.is_empty() {
            let ct = read_raw_frame(&mut self.stream)?;
            self.pending.clear();
            self.pending.resize(ct.len(), 0);
            let written = self
                .noise
                .read_message(&ct, &mut self.pending)
                .map_err(io::Error::other)?;
            self.pending.truncate(written);
        }
        let take = self.pending.len().min(buf.len());
        buf[..take].copy_from_slice(&self.pending[..take]);
        self.pending.drain(..take);
        Ok(take)
    }
}

impl Write for SecureStream {
    fn write(&mut self, data: &[u8]) -> io::Result<usize> {
        if data.is_empty() {
            return Ok(0);
        }
        let mut ct = vec![0u8; MAX_PLAINTEXT + TAG_LEN];
        for chunk in data.chunks(MAX_PLAINTEXT) {
            let n = self
                .noise
                .write_message(chunk, &mut ct)
                .map_err(io::Error::other)?;
            self.stream.write_all(&(n as u16).to_be_bytes())?;
            self.stream.write_all(&ct[..n])?;
        }
        self.stream.flush()?;
        Ok(data.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        self.stream.flush()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::TcpListener;

    fn linked_pair() -> (SecureStream, SecureStream) {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
        let addr = listener.local_addr().unwrap().to_string();
        let responder = std::thread::spawn(move || {
            let (stream, _) = listener.accept().expect("accept");
            establish_responder(stream, Some("t")).expect("responder handshake")
        });
        let initiator = TcpStream::connect(&addr).expect("connect");
        let client = establish_initiator(initiator, Some("t")).expect("initiator handshake");
        (client, responder.join().expect("responder thread"))
    }

    #[test]
    fn frames_round_trip_over_the_session() {
        let (mut a, mut b) = linked_pair();
        let payload = "a frame of any size up to 64 KiB".repeat(2_000);
        b.write_all(payload.as_bytes()).expect("write");
        let mut received = Vec::new();
        let mut buf = [0u8; 8192];
        while received.len() < payload.len() {
            let n = a.read(&mut buf).expect("read");
            assert!(n > 0);
            received.extend_from_slice(&buf[..n]);
        }
        assert_eq!(received, payload.as_bytes());
    }

    #[test]
    fn empty_reads_and_writes_are_noops() {
        let (mut a, mut b) = linked_pair();
        assert_eq!(a.write(b"").unwrap(), 0);
        assert_eq!(a.read(&mut []).unwrap(), 0);
        a.write_all(b"x").expect("write");
        let mut buf = [0u8; 1];
        let n = b.read(&mut buf).expect("read");
        assert_eq!(&buf[..n], b"x");
    }

    #[test]
    fn wrong_token_cannot_exchange_data() {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
        let addr = listener.local_addr().unwrap().to_string();
        let responder = std::thread::spawn(move || {
            let (stream, _) = listener.accept().expect("accept");
            establish_responder(stream, Some("right"))
        });
        let stream = TcpStream::connect(&addr).expect("connect");
        if let Ok(mut client) = establish_initiator(stream, Some("wrong")) {
            client.write_all(b"hello").expect("transport write");
            let mut server = responder.join().expect("responder").ok();
            if let Some(server) = server.as_mut() {
                let mut buf = [0u8; 5];
                assert!(
                    server.read_exact(&mut buf).is_err(),
                    "mismatched tokens must not deliver data"
                );
            }
        }
    }
}
