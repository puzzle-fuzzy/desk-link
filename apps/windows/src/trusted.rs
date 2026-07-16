use std::{
    collections::BTreeMap,
    fmt,
    fs::{self, OpenOptions},
    io::{self, Write},
    os::windows::ffi::OsStrExt,
    path::{Path, PathBuf},
    slice,
    sync::Mutex,
    time::{SystemTime, UNIX_EPOCH},
};

use blake2::{Blake2s256, Digest};
use desklink_crypto::{PairingError, PairingInvite, PeerIdentity, SessionId};
use ed25519_dalek::VerifyingKey;
use thiserror::Error;
use windows::{
    Win32::{
        Foundation::{HLOCAL, LocalFree},
        Security::Cryptography::{
            CRYPT_INTEGER_BLOB, CRYPTPROTECT_UI_FORBIDDEN, CryptProtectData, CryptUnprotectData,
        },
        Storage::FileSystem::{MOVEFILE_REPLACE_EXISTING, MOVEFILE_WRITE_THROUGH, MoveFileExW},
    },
    core::PCWSTR,
};
use zeroize::Zeroize;

use crate::{
    runtime::{ControllerAuthorization, ControllerAuthorizer},
    storage::local_app_data_path,
    window::{
        ApprovedController, PairingApprovalError, PairingApprovalGate, PendingController,
        PersistentApprovalGate, WindowsLocalApprovalDialog,
    },
};

const TRUST_MAGIC: &[u8; 8] = b"DLTRV1\0\0";
const TRUST_ENTRY_BYTES: usize = 16 + 32 + 8;
const MAX_TRUSTED_CONTROLLERS: usize = 64;
const FINGERPRINT_DOMAIN: &[u8] = b"desklink-controller-fingerprint-v1";

#[derive(Clone, Copy, Eq, Ord, PartialEq, PartialOrd)]
pub struct ControllerFingerprint([u8; 32]);

impl ControllerFingerprint {
    pub fn for_identity(identity: PeerIdentity) -> Self {
        Self::for_parts(identity.device_id(), identity.verify_key())
    }

    fn for_parts(device_id: [u8; 16], verify_key: VerifyingKey) -> Self {
        let mut hasher = Blake2s256::new();
        hasher.update(FINGERPRINT_DOMAIN);
        hasher.update(device_id);
        hasher.update(verify_key.as_bytes());
        let digest = hasher.finalize();
        let mut fingerprint = [0; 32];
        fingerprint.copy_from_slice(&digest);
        Self(fingerprint)
    }

    pub const fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }
}

impl fmt::Debug for ControllerFingerprint {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("ControllerFingerprint(")?;
        for byte in self.0 {
            write!(formatter, "{byte:02x}")?;
        }
        formatter.write_str(")")
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct TrustedController {
    device_id: [u8; 16],
    verify_key: VerifyingKey,
    approved_at_unix_s: u64,
}

impl TrustedController {
    pub const fn device_id(&self) -> [u8; 16] {
        self.device_id
    }

    pub fn verify_key(&self) -> VerifyingKey {
        self.verify_key
    }

    pub const fn approved_at_unix_s(&self) -> u64 {
        self.approved_at_unix_s
    }

    pub fn fingerprint(&self) -> ControllerFingerprint {
        ControllerFingerprint::for_parts(self.device_id, self.verify_key)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TrustStatus {
    Trusted(TrustedController),
    Unknown,
    KeyChanged { trusted: TrustedController },
}

#[derive(Debug, Error)]
pub enum WindowsTrustedControllerError {
    #[error("trusted-controller storage path is unavailable")]
    MissingStoragePath,
    #[error("trusted-controller file operation failed: {0}")]
    Io(#[from] io::Error),
    #[error("Windows trusted-controller protection failed: {0}")]
    Platform(#[from] windows::core::Error),
    #[error("protected trusted-controller data is corrupt or belongs to another user")]
    CorruptProtectedData,
    #[error("trusted-controller data is malformed")]
    CorruptStore,
    #[error("trusted-controller capacity has been reached")]
    CapacityReached,
    #[error("the device ID is already trusted with a different public key; revoke it first")]
    IdentityConflict,
}

#[derive(Clone, Debug)]
pub struct WindowsTrustedControllerStore {
    path: PathBuf,
}

/// Authorizes persisted trust first and optionally falls back to one explicitly
/// configured development key. The fallback never modifies the trust store.
#[derive(Clone, Debug)]
pub struct WindowsControllerAuthorizer {
    store: WindowsTrustedControllerStore,
    development_fallback: Option<VerifyingKey>,
}

impl WindowsControllerAuthorizer {
    pub const fn new(store: WindowsTrustedControllerStore) -> Self {
        Self {
            store,
            development_fallback: None,
        }
    }

    pub const fn with_development_fallback(
        store: WindowsTrustedControllerStore,
        expected: VerifyingKey,
    ) -> Self {
        Self {
            store,
            development_fallback: Some(expected),
        }
    }

    pub const fn store(&self) -> &WindowsTrustedControllerStore {
        &self.store
    }
}

impl ControllerAuthorizer for WindowsControllerAuthorizer {
    fn authorize(&self, identity: PeerIdentity) -> Result<ControllerAuthorization, String> {
        match self
            .store
            .status(identity)
            .map_err(|error| error.to_string())?
        {
            TrustStatus::Trusted(_) => Ok(ControllerAuthorization::Authorized),
            TrustStatus::KeyChanged { .. } => Ok(ControllerAuthorization::KeyChanged),
            TrustStatus::Unknown
                if self
                    .development_fallback
                    .is_some_and(|expected| expected == identity.verify_key()) =>
            {
                Ok(ControllerAuthorization::Authorized)
            }
            TrustStatus::Unknown => Ok(ControllerAuthorization::Unknown),
        }
    }
}

pub trait LocalControllerApproval: Send + Sync {
    fn approve(&self, pending: PendingController) -> bool;
}

impl LocalControllerApproval for WindowsLocalApprovalDialog {
    fn approve(&self, pending: PendingController) -> bool {
        Self::confirm(pending)
    }
}

/// Handles a live one-time pairing invitation. Already trusted controllers are
/// accepted first; an unknown controller is shown to the local approval
/// provider and persisted only if that exact authenticated identity is accepted.
pub struct WindowsPairingAuthorizer {
    store: WindowsTrustedControllerStore,
    invite: Mutex<PairingInvite>,
    gate: Mutex<PairingApprovalGate>,
    approval: Box<dyn LocalControllerApproval>,
}

impl WindowsPairingAuthorizer {
    pub fn new(
        store: WindowsTrustedControllerStore,
        invite: PairingInvite,
        approval: Box<dyn LocalControllerApproval>,
    ) -> Self {
        Self {
            store,
            invite: Mutex::new(invite),
            gate: Mutex::new(PairingApprovalGate::new()),
            approval,
        }
    }

    pub const fn store(&self) -> &WindowsTrustedControllerStore {
        &self.store
    }
}

impl ControllerAuthorizer for WindowsPairingAuthorizer {
    fn authorize(&self, identity: PeerIdentity) -> Result<ControllerAuthorization, String> {
        match self
            .store
            .status(identity)
            .map_err(|error| error.to_string())?
        {
            TrustStatus::Trusted(_) => return Ok(ControllerAuthorization::Authorized),
            TrustStatus::KeyChanged { .. } => return Ok(ControllerAuthorization::KeyChanged),
            TrustStatus::Unknown => {}
        }

        let started_at_unix_s = now_unix_s()?;
        let mut invite = self
            .invite
            .lock()
            .map_err(|_| "pairing invitation lock is poisoned".to_owned())?;
        let mut gate = self
            .gate
            .lock()
            .map_err(|_| "pairing approval lock is poisoned".to_owned())?;
        let pending = match gate.begin(&mut invite, identity, started_at_unix_s) {
            Ok(pending) => pending,
            Err(PairingApprovalError::Pairing(PairingError::Expired)) => {
                return Ok(ControllerAuthorization::Expired);
            }
            Err(error) => return Err(error.to_string()),
        };
        if !self.approval.approve(pending) {
            gate.reject(pending.identity())
                .map_err(|error| error.to_string())?;
            return Ok(ControllerAuthorization::Rejected);
        }
        let approved_at_unix_s = now_unix_s()?;
        let approved = match gate.approve(pending.identity(), approved_at_unix_s) {
            Ok(approved) => approved,
            Err(PairingApprovalError::Expired) => {
                return Ok(ControllerAuthorization::Expired);
            }
            Err(error) => return Err(error.to_string()),
        };
        self.store
            .trust(approved)
            .map_err(|error| error.to_string())?;
        Ok(ControllerAuthorization::Authorized)
    }
}

const PERSISTENT_APPROVAL_TTL_S: u64 = 120;

/// Authorizes access found through the host's fixed password. Existing trusted
/// controllers connect immediately; every unknown authenticated identity still
/// requires an explicit local Windows confirmation before trust is persisted.
pub struct WindowsPersistentAccessAuthorizer {
    store: WindowsTrustedControllerStore,
    gate: Mutex<PersistentApprovalGate>,
    approval: Box<dyn LocalControllerApproval>,
}

impl WindowsPersistentAccessAuthorizer {
    pub fn new(
        store: WindowsTrustedControllerStore,
        session_id: SessionId,
        approval: Box<dyn LocalControllerApproval>,
    ) -> Self {
        Self {
            store,
            gate: Mutex::new(PersistentApprovalGate::new(session_id)),
            approval,
        }
    }
}

impl ControllerAuthorizer for WindowsPersistentAccessAuthorizer {
    fn authorize(&self, identity: PeerIdentity) -> Result<ControllerAuthorization, String> {
        match self
            .store
            .status(identity)
            .map_err(|error| error.to_string())?
        {
            TrustStatus::Trusted(_) => return Ok(ControllerAuthorization::Authorized),
            TrustStatus::KeyChanged { .. } => return Ok(ControllerAuthorization::KeyChanged),
            TrustStatus::Unknown => {}
        }

        let mut gate = self
            .gate
            .lock()
            .map_err(|_| "fixed-access approval lock is poisoned".to_owned())?;
        // Another request may have approved this identity while this request
        // was waiting for the serialized local prompt.
        match self
            .store
            .status(identity)
            .map_err(|error| error.to_string())?
        {
            TrustStatus::Trusted(_) => return Ok(ControllerAuthorization::Authorized),
            TrustStatus::KeyChanged { .. } => return Ok(ControllerAuthorization::KeyChanged),
            TrustStatus::Unknown => {}
        }

        let started_at_unix_s = now_unix_s()?;
        let pending = gate
            .begin(identity, started_at_unix_s, PERSISTENT_APPROVAL_TTL_S)
            .map_err(|error| error.to_string())?;
        if !self.approval.approve(pending) {
            gate.reject(pending.identity())
                .map_err(|error| error.to_string())?;
            return Ok(ControllerAuthorization::Rejected);
        }
        let approved = match gate.approve(pending.identity(), now_unix_s()?) {
            Ok(approved) => approved,
            Err(PairingApprovalError::Expired) => return Ok(ControllerAuthorization::Expired),
            Err(error) => return Err(error.to_string()),
        };
        self.store
            .trust(approved)
            .map_err(|error| error.to_string())?;
        Ok(ControllerAuthorization::Authorized)
    }
}

fn now_unix_s() -> Result<u64, String> {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|_| "system clock is before the Unix epoch".to_owned())
        .map(|duration| duration.as_secs())
}

impl WindowsTrustedControllerStore {
    pub fn for_current_user() -> Result<Self, WindowsTrustedControllerError> {
        let local_app_data =
            local_app_data_path().ok_or(WindowsTrustedControllerError::MissingStoragePath)?;
        Ok(Self::new(
            local_app_data
                .join("DeskLink")
                .join("trusted-controllers.bin"),
        ))
    }

    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn list(&self) -> Result<Vec<TrustedController>, WindowsTrustedControllerError> {
        Ok(self.load_records()?.into_values().collect())
    }

    pub fn status(
        &self,
        identity: PeerIdentity,
    ) -> Result<TrustStatus, WindowsTrustedControllerError> {
        let records = self.load_records()?;
        let fingerprint = ControllerFingerprint::for_identity(identity);
        if let Some(record) = records.get(&fingerprint) {
            return Ok(TrustStatus::Trusted(*record));
        }
        Ok(records
            .values()
            .find(|record| record.device_id == identity.device_id())
            .copied()
            .map_or(TrustStatus::Unknown, |trusted| TrustStatus::KeyChanged {
                trusted,
            }))
    }

    pub fn trust(
        &self,
        approved: ApprovedController,
    ) -> Result<TrustedController, WindowsTrustedControllerError> {
        let mut records = self.load_records()?;
        if records.values().any(|record| {
            record.device_id == approved.device_id() && record.verify_key != approved.verify_key()
        }) {
            return Err(WindowsTrustedControllerError::IdentityConflict);
        }
        let record = TrustedController {
            device_id: approved.device_id(),
            verify_key: approved.verify_key(),
            approved_at_unix_s: approved.approved_at_unix_s(),
        };
        let fingerprint = record.fingerprint();
        if !records.contains_key(&fingerprint) && records.len() >= MAX_TRUSTED_CONTROLLERS {
            return Err(WindowsTrustedControllerError::CapacityReached);
        }
        records.insert(fingerprint, record);
        self.save_records(&records)?;
        Ok(record)
    }

    pub fn revoke(
        &self,
        fingerprint: ControllerFingerprint,
    ) -> Result<bool, WindowsTrustedControllerError> {
        let mut records = self.load_records()?;
        let removed = records.remove(&fingerprint).is_some();
        if removed {
            self.save_records(&records)?;
        }
        Ok(removed)
    }

    fn load_records(
        &self,
    ) -> Result<BTreeMap<ControllerFingerprint, TrustedController>, WindowsTrustedControllerError>
    {
        let protected = match fs::read(&self.path) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(BTreeMap::new()),
            Err(error) => return Err(error.into()),
        };
        let mut plaintext = unprotect(&protected)?;
        let parsed = decode_records(&plaintext);
        plaintext.zeroize();
        parsed
    }

    fn save_records(
        &self,
        records: &BTreeMap<ControllerFingerprint, TrustedController>,
    ) -> Result<(), WindowsTrustedControllerError> {
        let parent = self
            .path
            .parent()
            .ok_or(WindowsTrustedControllerError::MissingStoragePath)?;
        fs::create_dir_all(parent)?;
        let mut plaintext = encode_records(records)?;
        let protected = protect(&plaintext)?;
        plaintext.zeroize();

        let temporary = self.path.with_extension("tmp");
        let mut file = OpenOptions::new()
            .create(true)
            .truncate(true)
            .write(true)
            .open(&temporary)?;
        file.write_all(&protected)?;
        file.sync_all()?;
        drop(file);
        replace_file(&temporary, &self.path)?;
        Ok(())
    }
}

fn encode_records(
    records: &BTreeMap<ControllerFingerprint, TrustedController>,
) -> Result<Vec<u8>, WindowsTrustedControllerError> {
    if records.len() > MAX_TRUSTED_CONTROLLERS {
        return Err(WindowsTrustedControllerError::CapacityReached);
    }
    let mut bytes = Vec::with_capacity(TRUST_MAGIC.len() + 4 + records.len() * TRUST_ENTRY_BYTES);
    bytes.extend_from_slice(TRUST_MAGIC);
    bytes.extend_from_slice(&(records.len() as u32).to_be_bytes());
    for record in records.values() {
        bytes.extend_from_slice(&record.device_id);
        bytes.extend_from_slice(record.verify_key.as_bytes());
        bytes.extend_from_slice(&record.approved_at_unix_s.to_be_bytes());
    }
    Ok(bytes)
}

fn decode_records(
    bytes: &[u8],
) -> Result<BTreeMap<ControllerFingerprint, TrustedController>, WindowsTrustedControllerError> {
    if bytes.len() < TRUST_MAGIC.len() + 4 || &bytes[..TRUST_MAGIC.len()] != TRUST_MAGIC {
        return Err(WindowsTrustedControllerError::CorruptStore);
    }
    let count = u32::from_be_bytes(
        bytes[TRUST_MAGIC.len()..TRUST_MAGIC.len() + 4]
            .try_into()
            .map_err(|_| WindowsTrustedControllerError::CorruptStore)?,
    ) as usize;
    if count > MAX_TRUSTED_CONTROLLERS
        || bytes.len() != TRUST_MAGIC.len() + 4 + count * TRUST_ENTRY_BYTES
    {
        return Err(WindowsTrustedControllerError::CorruptStore);
    }
    let mut records = BTreeMap::new();
    for entry in bytes[TRUST_MAGIC.len() + 4..].chunks_exact(TRUST_ENTRY_BYTES) {
        let device_id = entry[..16]
            .try_into()
            .map_err(|_| WindowsTrustedControllerError::CorruptStore)?;
        let verify_key = VerifyingKey::from_bytes(
            entry[16..48]
                .try_into()
                .map_err(|_| WindowsTrustedControllerError::CorruptStore)?,
        )
        .map_err(|_| WindowsTrustedControllerError::CorruptStore)?;
        let approved_at_unix_s = u64::from_be_bytes(
            entry[48..]
                .try_into()
                .map_err(|_| WindowsTrustedControllerError::CorruptStore)?,
        );
        let record = TrustedController {
            device_id,
            verify_key,
            approved_at_unix_s,
        };
        if records.insert(record.fingerprint(), record).is_some()
            || records
                .values()
                .filter(|existing| existing.device_id == device_id)
                .count()
                != 1
        {
            return Err(WindowsTrustedControllerError::CorruptStore);
        }
    }
    Ok(records)
}

fn protect(plaintext: &[u8]) -> Result<Vec<u8>, WindowsTrustedControllerError> {
    let input = blob_for(plaintext)?;
    let mut output = CRYPT_INTEGER_BLOB::default();
    unsafe {
        CryptProtectData(
            &input,
            PCWSTR::null(),
            None,
            None,
            None,
            CRYPTPROTECT_UI_FORBIDDEN,
            &mut output,
        )?;
    }
    copy_and_free(output)
}

fn unprotect(protected: &[u8]) -> Result<Vec<u8>, WindowsTrustedControllerError> {
    let input = blob_for(protected)?;
    let mut output = CRYPT_INTEGER_BLOB::default();
    unsafe {
        CryptUnprotectData(
            &input,
            None,
            None,
            None,
            None,
            CRYPTPROTECT_UI_FORBIDDEN,
            &mut output,
        )
        .map_err(|_| WindowsTrustedControllerError::CorruptProtectedData)?;
    }
    copy_and_free(output)
}

fn blob_for(bytes: &[u8]) -> Result<CRYPT_INTEGER_BLOB, WindowsTrustedControllerError> {
    Ok(CRYPT_INTEGER_BLOB {
        cbData: u32::try_from(bytes.len())
            .map_err(|_| WindowsTrustedControllerError::CorruptStore)?,
        pbData: bytes.as_ptr().cast_mut(),
    })
}

fn copy_and_free(blob: CRYPT_INTEGER_BLOB) -> Result<Vec<u8>, WindowsTrustedControllerError> {
    if blob.pbData.is_null() && blob.cbData != 0 {
        return Err(WindowsTrustedControllerError::CorruptProtectedData);
    }
    let bytes = if blob.cbData == 0 {
        Vec::new()
    } else {
        unsafe { slice::from_raw_parts(blob.pbData, blob.cbData as usize) }.to_vec()
    };
    if !blob.pbData.is_null() {
        unsafe {
            let _ = LocalFree(Some(HLOCAL(blob.pbData.cast())));
        }
    }
    Ok(bytes)
}

fn replace_file(source: &Path, destination: &Path) -> Result<(), WindowsTrustedControllerError> {
    let source = source
        .as_os_str()
        .encode_wide()
        .chain(Some(0))
        .collect::<Vec<_>>();
    let destination = destination
        .as_os_str()
        .encode_wide()
        .chain(Some(0))
        .collect::<Vec<_>>();
    unsafe {
        MoveFileExW(
            PCWSTR(source.as_ptr()),
            PCWSTR(destination.as_ptr()),
            MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH,
        )?;
    }
    Ok(())
}
