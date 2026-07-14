use blake2::{Blake2s256, Digest};
use chacha20poly1305::{
    ChaCha20Poly1305, KeyInit,
    aead::{AeadInPlace, generic_array::GenericArray},
};
use rand_core::{CryptoRng, OsRng, RngCore};
use snow::{
    Error,
    params::{CipherChoice, DHChoice, HashChoice},
    resolvers::CryptoResolver,
    types::{Cipher, Dh, Hash, Random},
};
use x25519_dalek::{PublicKey, StaticSecret};
use zeroize::{Zeroize, ZeroizeOnDrop, Zeroizing};

const KEY_BYTES: usize = 32;
const TAG_BYTES: usize = 16;
const BLAKE2S_BLOCK_BYTES: usize = 64;
const BLAKE2S_HASH_BYTES: usize = 32;
const MAX_HASH_INPUT_BYTES: usize = 65_535 + BLAKE2S_HASH_BYTES;

#[derive(Default)]
pub(crate) struct DesklinkResolver;

impl CryptoResolver for DesklinkResolver {
    fn resolve_rng(&self) -> Option<Box<dyn Random>> {
        Some(Box::new(ResolverRng::default()))
    }

    fn resolve_dh(&self, choice: &DHChoice) -> Option<Box<dyn Dh>> {
        match choice {
            DHChoice::Curve25519 => Some(Box::new(ZeroizingDh25519::default())),
            _ => None,
        }
    }

    fn resolve_hash(&self, choice: &HashChoice) -> Option<Box<dyn Hash>> {
        match choice {
            HashChoice::Blake2s => Some(Box::new(ZeroizingBlake2s::default())),
            _ => None,
        }
    }

    fn resolve_cipher(&self, choice: &CipherChoice) -> Option<Box<dyn Cipher>> {
        match choice {
            CipherChoice::ChaChaPoly => Some(Box::new(ZeroizingChaChaPoly::default())),
            _ => None,
        }
    }
}

#[derive(Default)]
struct ResolverRng(OsRng);

impl RngCore for ResolverRng {
    fn next_u32(&mut self) -> u32 {
        self.0.next_u32()
    }

    fn next_u64(&mut self) -> u64 {
        self.0.next_u64()
    }

    fn fill_bytes(&mut self, destination: &mut [u8]) {
        self.0.fill_bytes(destination);
    }

    fn try_fill_bytes(&mut self, destination: &mut [u8]) -> Result<(), rand_core::Error> {
        self.0.try_fill_bytes(destination)
    }
}

impl CryptoRng for ResolverRng {}
impl Random for ResolverRng {}

#[derive(Zeroize, ZeroizeOnDrop)]
pub(crate) struct ZeroizingDh25519 {
    private: [u8; KEY_BYTES],
    public: [u8; KEY_BYTES],
}

impl Default for ZeroizingDh25519 {
    fn default() -> Self {
        Self {
            private: [0; KEY_BYTES],
            public: [0; KEY_BYTES],
        }
    }
}

impl ZeroizingDh25519 {
    fn derive_public(&mut self) {
        let private = StaticSecret::from(self.private);
        self.public = PublicKey::from(&private).to_bytes();
    }
}

impl Dh for ZeroizingDh25519 {
    fn name(&self) -> &'static str {
        "25519"
    }

    fn pub_len(&self) -> usize {
        KEY_BYTES
    }

    fn priv_len(&self) -> usize {
        KEY_BYTES
    }

    fn set(&mut self, private_key: &[u8]) {
        assert_eq!(
            private_key.len(),
            KEY_BYTES,
            "invalid X25519 private key length"
        );
        self.private.zeroize();
        self.private.copy_from_slice(private_key);
        self.derive_public();
    }

    fn generate(&mut self, rng: &mut dyn Random) {
        rng.fill_bytes(&mut self.private);
        self.derive_public();
    }

    fn pubkey(&self) -> &[u8] {
        &self.public
    }

    fn privkey(&self) -> &[u8] {
        &self.private
    }

    fn dh(&self, public_key: &[u8], output: &mut [u8]) -> Result<(), Error> {
        if public_key.len() < KEY_BYTES || output.len() < KEY_BYTES {
            return Err(Error::Dh);
        }
        let public_bytes: [u8; KEY_BYTES] =
            public_key[..KEY_BYTES].try_into().map_err(|_| Error::Dh)?;
        let private = StaticSecret::from(self.private);
        let public = PublicKey::from(public_bytes);
        let shared = private.diffie_hellman(&public);
        if !shared.was_contributory() {
            return Err(Error::Dh);
        }
        output[..KEY_BYTES].copy_from_slice(shared.as_bytes());
        Ok(())
    }
}

#[derive(Zeroize, ZeroizeOnDrop)]
pub(crate) struct ZeroizingChaChaPoly {
    key: [u8; KEY_BYTES],
}

impl Default for ZeroizingChaChaPoly {
    fn default() -> Self {
        Self {
            key: [0; KEY_BYTES],
        }
    }
}

impl ZeroizingChaChaPoly {
    fn cipher(&self) -> ChaCha20Poly1305 {
        ChaCha20Poly1305::new_from_slice(&self.key).expect("fixed-length ChaChaPoly key")
    }

    fn nonce(nonce: u64) -> [u8; 12] {
        let mut bytes = [0; 12];
        bytes[4..].copy_from_slice(&nonce.to_le_bytes());
        bytes
    }
}

impl Cipher for ZeroizingChaChaPoly {
    fn name(&self) -> &'static str {
        "ChaChaPoly"
    }

    fn set(&mut self, key: &[u8]) {
        assert_eq!(key.len(), KEY_BYTES, "invalid ChaChaPoly key length");
        self.key.zeroize();
        self.key.copy_from_slice(key);
    }

    fn encrypt(&self, nonce: u64, authtext: &[u8], plaintext: &[u8], output: &mut [u8]) -> usize {
        assert!(output.len() >= plaintext.len() + TAG_BYTES);
        output[..plaintext.len()].copy_from_slice(plaintext);
        let nonce = Self::nonce(nonce);
        let tag = self
            .cipher()
            .encrypt_in_place_detached(
                GenericArray::from_slice(&nonce),
                authtext,
                &mut output[..plaintext.len()],
            )
            .expect("Noise validates ChaChaPoly encryption lengths");
        output[plaintext.len()..plaintext.len() + TAG_BYTES].copy_from_slice(&tag);
        plaintext.len() + TAG_BYTES
    }

    fn decrypt(
        &self,
        nonce: u64,
        authtext: &[u8],
        ciphertext: &[u8],
        output: &mut [u8],
    ) -> Result<usize, Error> {
        if ciphertext.len() < TAG_BYTES || output.len() < ciphertext.len() - TAG_BYTES {
            return Err(Error::Decrypt);
        }
        let plaintext_len = ciphertext.len() - TAG_BYTES;
        output[..plaintext_len].copy_from_slice(&ciphertext[..plaintext_len]);
        let nonce = Self::nonce(nonce);
        let result = self.cipher().decrypt_in_place_detached(
            GenericArray::from_slice(&nonce),
            authtext,
            &mut output[..plaintext_len],
            GenericArray::from_slice(&ciphertext[plaintext_len..]),
        );
        if result.is_err() {
            output[..plaintext_len].zeroize();
            return Err(Error::Decrypt);
        }
        Ok(plaintext_len)
    }
}

#[derive(Zeroize, ZeroizeOnDrop)]
pub(crate) struct ZeroizingBlake2s {
    input: Vec<u8>,
}

impl Default for ZeroizingBlake2s {
    fn default() -> Self {
        Self {
            input: Vec::with_capacity(MAX_HASH_INPUT_BYTES),
        }
    }
}

impl Hash for ZeroizingBlake2s {
    fn name(&self) -> &'static str {
        "BLAKE2s"
    }

    fn block_len(&self) -> usize {
        BLAKE2S_BLOCK_BYTES
    }

    fn hash_len(&self) -> usize {
        BLAKE2S_HASH_BYTES
    }

    fn reset(&mut self) {
        self.input.zeroize();
    }

    fn input(&mut self, data: &[u8]) {
        let new_length = self
            .input
            .len()
            .checked_add(data.len())
            .expect("BLAKE2s input length overflow");
        assert!(
            new_length <= MAX_HASH_INPUT_BYTES,
            "Noise hash input is too large"
        );
        self.input.extend_from_slice(data);
    }

    fn result(&mut self, output: &mut [u8]) {
        assert!(output.len() >= BLAKE2S_HASH_BYTES);
        let mut digest = Blake2s256::digest(&self.input);
        output[..BLAKE2S_HASH_BYTES].copy_from_slice(&digest);
        digest.as_mut_slice().zeroize();
        self.reset();
    }

    fn hmac(&mut self, key: &[u8], data: &[u8], output: &mut [u8]) {
        // This is Snow 0.9.6's Hash::hmac algorithm specialized to BLAKE2s,
        // with every key-derived temporary placed in a zeroizing holder.
        assert!(key.len() <= BLAKE2S_BLOCK_BYTES);
        let mut ipad = Zeroizing::new([0x36; BLAKE2S_BLOCK_BYTES]);
        let mut opad = Zeroizing::new([0x5c; BLAKE2S_BLOCK_BYTES]);
        for index in 0..key.len() {
            ipad[index] ^= key[index];
            opad[index] ^= key[index];
        }
        self.reset();
        self.input(&ipad[..]);
        self.input(data);
        let mut inner = Zeroizing::new([0; BLAKE2S_HASH_BYTES]);
        self.result(&mut inner[..]);
        self.input(&opad[..]);
        self.input(&inner[..]);
        self.result(output);
    }

    fn hkdf(
        &mut self,
        chaining_key: &[u8],
        input_key_material: &[u8],
        outputs: usize,
        output_1: &mut [u8],
        output_2: &mut [u8],
        output_3: &mut [u8],
    ) {
        // This mirrors Snow 0.9.6's Hash::hkdf expansion while ensuring its
        // temporary key and chained inputs are scrubbed on every return path.
        assert!((1..=3).contains(&outputs));
        let mut temporary_key = Zeroizing::new([0; BLAKE2S_HASH_BYTES]);
        self.hmac(chaining_key, input_key_material, &mut temporary_key[..]);
        self.hmac(&temporary_key[..], &[1], output_1);
        if outputs == 1 {
            return;
        }
        let mut input_2 = Zeroizing::new([0; BLAKE2S_HASH_BYTES + 1]);
        input_2[..BLAKE2S_HASH_BYTES].copy_from_slice(&output_1[..BLAKE2S_HASH_BYTES]);
        input_2[BLAKE2S_HASH_BYTES] = 2;
        self.hmac(&temporary_key[..], &input_2[..], output_2);
        if outputs == 2 {
            return;
        }
        let mut input_3 = Zeroizing::new([0; BLAKE2S_HASH_BYTES + 1]);
        input_3[..BLAKE2S_HASH_BYTES].copy_from_slice(&output_2[..BLAKE2S_HASH_BYTES]);
        input_3[BLAKE2S_HASH_BYTES] = 3;
        self.hmac(&temporary_key[..], &input_3[..], output_3);
    }
}

#[cfg(test)]
mod tests {
    use snow::{
        params::{CipherChoice, DHChoice, HashChoice},
        resolvers::CryptoResolver,
        types::{Cipher, Dh, Hash},
    };
    use zeroize::ZeroizeOnDrop;

    use super::{
        BLAKE2S_HASH_BYTES, DesklinkResolver, MAX_HASH_INPUT_BYTES, ZeroizingBlake2s,
        ZeroizingChaChaPoly, ZeroizingDh25519,
    };

    #[test]
    fn resolver_secret_holders_zeroize_on_drop() {
        fn assert_zeroize_on_drop<T: ZeroizeOnDrop>() {}

        assert_zeroize_on_drop::<ZeroizingDh25519>();
        assert_zeroize_on_drop::<ZeroizingChaChaPoly>();
        assert_zeroize_on_drop::<ZeroizingBlake2s>();
    }

    #[test]
    fn resolver_exposes_only_the_exact_noise_primitives() {
        let resolver = DesklinkResolver;

        assert!(resolver.resolve_rng().is_some());
        assert!(resolver.resolve_dh(&DHChoice::Curve25519).is_some());
        assert!(resolver.resolve_cipher(&CipherChoice::ChaChaPoly).is_some());
        assert!(resolver.resolve_hash(&HashChoice::Blake2s).is_some());
        assert!(resolver.resolve_cipher(&CipherChoice::AESGCM).is_none());
        assert!(resolver.resolve_hash(&HashChoice::SHA256).is_none());
    }

    #[test]
    fn zeroizing_x25519_adapter_agrees_on_shared_secret() {
        let mut alice = ZeroizingDh25519::default();
        let mut bob = ZeroizingDh25519::default();
        alice.set(&[7; 32]);
        bob.set(&[9; 32]);
        let alice_public = alice.pubkey().to_vec();
        let bob_public = bob.pubkey().to_vec();
        let mut alice_shared = [0; 32];
        let mut bob_shared = [0; 32];

        alice.dh(&bob_public, &mut alice_shared).unwrap();
        bob.dh(&alice_public, &mut bob_shared).unwrap();

        assert_eq!(alice_shared, bob_shared);
        assert_ne!(alice_shared, [0; 32]);
    }

    #[test]
    fn x25519_adapter_accepts_snow_max_dh_scratch_buffers() {
        let mut alice = ZeroizingDh25519::default();
        let mut bob = ZeroizingDh25519::default();
        alice.set(&[13; 32]);
        bob.set(&[17; 32]);
        let mut bob_public = [0; 56];
        bob_public[..32].copy_from_slice(bob.pubkey());
        let mut shared = [0; 56];

        alice.dh(&bob_public, &mut shared).unwrap();

        assert_ne!(shared[..32], [0; 32]);
        assert_eq!(shared[32..], [0; 24]);
    }

    #[test]
    fn zeroizing_chachapoly_adapter_authenticates_ciphertext() {
        let mut cipher = ZeroizingChaChaPoly::default();
        cipher.set(&[11; 32]);
        let mut ciphertext = [0; 32];
        let written = cipher.encrypt(5, b"transcript", b"desklink", &mut ciphertext);
        let mut plaintext = [0; 16];

        assert_eq!(
            cipher
                .decrypt(5, b"transcript", &ciphertext[..written], &mut plaintext)
                .unwrap(),
            8
        );
        assert_eq!(&plaintext[..8], b"desklink");

        ciphertext[written - 1] ^= 1;
        assert!(
            cipher
                .decrypt(5, b"transcript", &ciphertext[..written], &mut plaintext)
                .is_err()
        );
    }

    #[test]
    fn zeroizing_blake2s_adapter_matches_known_digest() {
        let mut hash = ZeroizingBlake2s::default();
        let mut output = [0; 32];
        hash.input(b"abc");
        hash.result(&mut output);

        assert_eq!(
            hex::encode(output),
            "508c5e8c327c14e2e1a72ba34eeb452f37458b209ed63a294d999b4c86675982"
        );
    }

    #[test]
    fn blake2s_retained_buffer_never_reallocates_at_noise_maximum() {
        let mut hash = ZeroizingBlake2s::default();
        let allocation = hash.input.as_ptr();
        let prefix = [0; BLAKE2S_HASH_BYTES];
        let maximum_message = vec![0; MAX_HASH_INPUT_BYTES - prefix.len()];

        hash.input(&prefix);
        hash.input(&maximum_message);

        assert_eq!(hash.input.capacity(), MAX_HASH_INPUT_BYTES);
        assert_eq!(hash.input.as_ptr(), allocation);
    }
}
