//! The cipher of Govee's encrypted command channel.
//!
//! A light whose advertisement carries the encrypt flag expects every frame
//! encrypted and answers encrypted: AES-128 in ECB mode, no padding, on each
//! whole 16-byte block, then RC4 with the same key on the trailing remainder
//! (bytes 16-19 of a 20-byte frame). The handshake runs under a published
//! pre-shared key and yields a session key the rest of the session uses.

use aes::Aes128;
use aes::cipher::generic_array::GenericArray;
use aes::cipher::{BlockDecrypt, BlockEncrypt, KeyInit};

/// Length of every key, in bytes.
pub const KEY_LEN: usize = 16;
/// The pre-shared key the handshake runs under, published by the community
/// implementations.
pub const PRE_SHARED_KEY: [u8; KEY_LEN] = *b"MakingLifeSmarte";
/// AES block length.
const BLOCK_LEN: usize = 16;
/// RC4 state length.
const RC4_STATE_LEN: usize = 256;

/// The transform bound to one key: AES for whole blocks, RC4 for the rest.
pub struct FrameCipher {
    aes: Aes128,
    schedule: [u8; RC4_STATE_LEN],
}

impl FrameCipher {
    /// Prepares the AES key and the RC4 key schedule.
    #[must_use]
    pub fn new(key: &[u8; KEY_LEN]) -> Self {
        Self {
            aes: Aes128::new(GenericArray::from_slice(key)),
            schedule: rc4_schedule(key),
        }
    }

    /// Plaintext frame to wire bytes.
    #[must_use]
    pub fn encrypt(&self, plain: &[u8]) -> Vec<u8> {
        self.transform(plain, |block| self.aes.encrypt_block(block))
    }

    /// Wire bytes to plaintext frame.
    #[must_use]
    pub fn decrypt(&self, wire: &[u8]) -> Vec<u8> {
        self.transform(wire, |block| self.aes.decrypt_block(block))
    }

    fn transform(
        &self,
        data: &[u8],
        block_op: impl Fn(&mut GenericArray<u8, aes::cipher::typenum::U16>),
    ) -> Vec<u8> {
        let mut out = Vec::with_capacity(data.len());
        let (chunks, remainder) = data.as_chunks::<BLOCK_LEN>();
        for chunk in chunks {
            let mut block = GenericArray::clone_from_slice(chunk);
            block_op(&mut block);
            out.extend_from_slice(&block);
        }
        if !remainder.is_empty() {
            out.extend(self.rc4(remainder));
        }
        out
    }

    /// RC4 keystream XOR from the start of a fresh copy of the schedule, as
    /// the reference implementation restarts it per frame.
    fn rc4(&self, data: &[u8]) -> Vec<u8> {
        let mut state = self.schedule;
        let mut i: u8 = 0;
        let mut j: u8 = 0;
        data.iter()
            .map(|byte| {
                i = i.wrapping_add(1);
                j = j.wrapping_add(state[usize::from(i)]);
                state.swap(usize::from(i), usize::from(j));
                let k =
                    state[usize::from(state[usize::from(i)].wrapping_add(state[usize::from(j)]))];
                byte ^ k
            })
            .collect()
    }
}

/// RC4 key-scheduling algorithm.
fn rc4_schedule(key: &[u8; KEY_LEN]) -> [u8; RC4_STATE_LEN] {
    let mut state = [0u8; RC4_STATE_LEN];
    for (i, slot) in state.iter_mut().enumerate() {
        *slot = u8::try_from(i).expect("index below 256");
    }
    let mut j: u8 = 0;
    for i in 0..RC4_STATE_LEN {
        j = j.wrapping_add(state[i]).wrapping_add(key[i % KEY_LEN]);
        state.swap(i, usize::from(j));
    }
    state
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(hex: &str) -> Vec<u8> {
        (0..hex.len() / 2)
            .map(|i| u8::from_str_radix(&hex[i * 2..i * 2 + 2], 16).unwrap())
            .collect()
    }

    #[test]
    fn whole_block_is_aes_ecb() {
        // FIPS-197 appendix C.1 vector.
        let key: [u8; KEY_LEN] = parse("000102030405060708090a0b0c0d0e0f")
            .try_into()
            .unwrap();
        let cipher = FrameCipher::new(&key);
        let plain = parse("00112233445566778899aabbccddeeff");
        let wire = cipher.encrypt(&plain);
        assert_eq!(wire, parse("69c4e0d86a7b0430d8cdb78070b4c55a"));
        assert_eq!(cipher.decrypt(&wire), plain);
    }

    #[test]
    fn remainder_is_rc4_with_the_same_key() {
        // Vector computed with the reference RC4 (key schedule + keystream) for
        // the pre-shared key and the first four handshake bytes.
        let cipher = FrameCipher::new(&PRE_SHARED_KEY);
        let plain = [0xe7, 0x01, 0x02, 0x03];
        let wire = cipher.encrypt(&plain);
        assert_eq!(wire, parse("8fef425c"));
        assert_eq!(cipher.decrypt(&wire), plain);
    }

    #[test]
    fn twenty_byte_frame_round_trips() {
        let cipher = FrameCipher::new(&PRE_SHARED_KEY);
        let plain: Vec<u8> = (0..20).collect();
        let wire = cipher.encrypt(&plain);
        assert_eq!(wire.len(), 20);
        assert_eq!(&wire[..16], &cipher.encrypt(&plain[..16])[..]);
        assert_eq!(cipher.decrypt(&wire), plain);
    }
}
