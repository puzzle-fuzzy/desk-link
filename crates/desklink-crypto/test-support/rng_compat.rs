//! Test-only bridge from rand_chacha 0.10 to the public rand_core 0.6 API.
//! Production entropy continues to use the existing OS RNG boundary.

use rand_chacha::rand_core::{Rng as CurrentRng, SeedableRng as CurrentSeedableRng};
use rand_core::{CryptoRng, RngCore, SeedableRng};

pub(crate) struct ChaCha20Rng(rand_chacha::ChaCha20Rng);

impl SeedableRng for ChaCha20Rng {
    type Seed = [u8; 32];

    fn from_seed(seed: Self::Seed) -> Self {
        Self(<rand_chacha::ChaCha20Rng as CurrentSeedableRng>::from_seed(
            seed,
        ))
    }
}

impl RngCore for ChaCha20Rng {
    fn next_u32(&mut self) -> u32 {
        CurrentRng::next_u32(&mut self.0)
    }

    fn next_u64(&mut self) -> u64 {
        CurrentRng::next_u64(&mut self.0)
    }

    fn fill_bytes(&mut self, destination: &mut [u8]) {
        CurrentRng::fill_bytes(&mut self.0, destination);
    }

    fn try_fill_bytes(&mut self, destination: &mut [u8]) -> Result<(), rand_core::Error> {
        self.fill_bytes(destination);
        Ok(())
    }
}

impl CryptoRng for ChaCha20Rng {}

#[test]
fn legacy_rng_bridge_preserves_the_chacha20_reference_block() {
    let mut rng = ChaCha20Rng::from_seed([0; 32]);
    let mut actual = [0; 64];
    rng.fill_bytes(&mut actual);
    // Public all-zero ChaCha20 reference block, independent of either trait API.
    assert_eq!(
        actual,
        [
            0x76, 0xb8, 0xe0, 0xad, 0xa0, 0xf1, 0x3d, 0x90, 0x40, 0x5d, 0x6a, 0xe5, 0x53, 0x86,
            0xbd, 0x28, 0xbd, 0xd2, 0x19, 0xb8, 0xa0, 0x8d, 0xed, 0x1a, 0xa8, 0x36, 0xef, 0xcc,
            0x8b, 0x77, 0x0d, 0xc7, 0xda, 0x41, 0x59, 0x7c, 0x51, 0x57, 0x48, 0x8d, 0x77, 0x24,
            0xe0, 0x3f, 0xb8, 0xd8, 0x4a, 0x37, 0x6a, 0x43, 0xb8, 0xf4, 0x15, 0x18, 0xa1, 0x1c,
            0xc3, 0x87, 0xb6, 0x69, 0xb2, 0xee, 0x65, 0x86,
        ]
    );
}
