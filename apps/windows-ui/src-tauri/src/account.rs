use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use apps_windows::{
    account::{AccountSession, WindowsAccountSessionStore},
    identity::WindowsIdentityStore,
};
use rand_core::OsRng;
use reqwest::Client;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AccountSnapshot {
    pub signed_in: bool,
    pub user: Option<AccountUser>,
    pub device_id: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AccountUser {
    pub id: String,
    pub email: String,
    pub email_verified: bool,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct AccountTokens {
    access_token: String,
    refresh_token: String,
}

#[derive(Debug, Deserialize)]
struct LoginResponse {
    user: AccountUser,
    tokens: AccountTokens,
}

#[derive(Debug, Deserialize)]
struct RefreshResponse {
    tokens: AccountTokens,
}

#[derive(Debug, Deserialize)]
struct MeResponse {
    user: AccountUser,
}

#[derive(Debug, Deserialize)]
struct ErrorResponse {
    message: Option<String>,
}

const ACCOUNT_REQUEST_TIMEOUT: Duration = Duration::from_secs(12);
const SUPERSEDED_OPERATION: &str = "账号操作已被新的选择替代。";

#[derive(Default)]
struct OperationState {
    generation: u64,
    active: bool,
    pending_session: Option<AccountSession>,
    // Retain the old session until the frontend confirms workspace ownership.
    previous_session: Option<Option<AccountSession>>,
}

#[derive(Clone, Default)]
pub struct AccountOperationOwner {
    state: Arc<Mutex<OperationState>>,
}

impl AccountOperationOwner {
    pub async fn after_wait<T>(
        &self,
        generation: u64,
        wait: impl std::future::Future<Output = ()>,
        action: impl FnOnce() -> Result<T, String>,
    ) -> Result<T, String> {
        self.with_current(generation, || Ok(()))?;
        wait.await;
        self.with_current(generation, action)
    }

    pub fn with_current<T>(
        &self,
        generation: u64,
        action: impl FnOnce() -> Result<T, String>,
    ) -> Result<T, String> {
        let state = self
            .state
            .lock()
            .map_err(|_| SUPERSEDED_OPERATION.to_owned())?;
        if !state.active || state.generation != generation {
            return Err(SUPERSEDED_OPERATION.to_owned());
        }
        action()
    }
}

#[derive(Clone)]
pub struct AccountManager {
    store: Option<WindowsAccountSessionStore>,
    identity_store: Option<WindowsIdentityStore>,
    operation: AccountOperationOwner,
    client: Option<Client>,
    base_url: String,
}

impl AccountManager {
    pub fn for_current_user() -> Self {
        let store = WindowsAccountSessionStore::for_current_user().ok();
        let client = Client::builder()
            .timeout(ACCOUNT_REQUEST_TIMEOUT)
            .build()
            .ok();
        Self {
            store,
            identity_store: WindowsIdentityStore::for_current_user().ok(),
            operation: AccountOperationOwner::default(),
            client,
            base_url: account_base_url(),
        }
    }

    pub fn operation_owner(&self) -> AccountOperationOwner {
        self.operation.clone()
    }

    fn rollback_session(&self, state: &mut OperationState) -> Result<(), String> {
        if let Some(previous) = state.previous_session.as_ref() {
            let store = self
                .store
                .as_ref()
                .ok_or_else(|| "账号会话存储不可用。".to_owned())?;
            match previous {
                Some(session) => store.save(session).map_err(|error| error.to_string())?,
                None => {
                    store.clear().map_err(|error| error.to_string())?;
                }
            }
            state.previous_session = None;
        }
        state.pending_session = None;
        Ok(())
    }

    fn begin_operation(&self, generation: u64) -> Result<(), String> {
        let mut state = self
            .operation
            .state
            .lock()
            .map_err(|_| SUPERSEDED_OPERATION.to_owned())?;
        if generation <= state.generation {
            return Err(SUPERSEDED_OPERATION.to_owned());
        }
        state.generation = generation;
        state.active = false;
        self.rollback_session(&mut state)?;
        state.active = true;
        Ok(())
    }

    pub fn cancel_operation(&self, generation: u64) -> Result<(), String> {
        let mut state = self
            .operation
            .state
            .lock()
            .map_err(|_| SUPERSEDED_OPERATION.to_owned())?;
        if generation < state.generation {
            return Ok(());
        }
        state.generation = generation;
        state.active = false;
        self.rollback_session(&mut state)?;
        Ok(())
    }

    fn stage_session(&self, generation: u64, session: AccountSession) -> Result<(), String> {
        let mut state = self
            .operation
            .state
            .lock()
            .map_err(|_| SUPERSEDED_OPERATION.to_owned())?;
        if !state.active || generation != state.generation {
            return Err(SUPERSEDED_OPERATION.to_owned());
        }
        state.pending_session = Some(session);
        Ok(())
    }

    pub fn accept_operation(&self, generation: u64) -> Result<(), String> {
        let mut state = self
            .operation
            .state
            .lock()
            .map_err(|_| SUPERSEDED_OPERATION.to_owned())?;
        if !state.active || generation != state.generation {
            return Err(SUPERSEDED_OPERATION.to_owned());
        }
        let Some(session) = state.pending_session.as_ref() else {
            return Err("账号操作没有待确认的会话。".to_owned());
        };
        let store = self
            .store
            .as_ref()
            .ok_or_else(|| "账号会话存储不可用。".to_owned())?;
        let previous = store.load().map_err(|error| error.to_string())?;
        store.save(session).map_err(|error| error.to_string())?;
        state.previous_session = Some(previous);
        state.pending_session = None;
        Ok(())
    }

    pub fn finish_operation(&self, generation: u64) -> Result<(), String> {
        let mut state = self
            .operation
            .state
            .lock()
            .map_err(|_| SUPERSEDED_OPERATION.to_owned())?;
        if !state.active || generation != state.generation {
            return Err(SUPERSEDED_OPERATION.to_owned());
        }
        state.previous_session = None;
        state.active = false;
        Ok(())
    }

    pub async fn restore(&self, generation: u64) -> Result<AccountSnapshot, String> {
        self.begin_operation(generation)?;
        let Some(store) = self.store.as_ref() else {
            return Ok(AccountSnapshot::signed_out());
        };
        let Some(mut session) = store.load().map_err(|error| error.to_string())? else {
            return Ok(AccountSnapshot::signed_out());
        };
        match self
            .request("GET", "v1/account/me", None, Some(&session.access_token))
            .await
        {
            Ok(body) => {
                let response: MeResponse = serde_json::from_value(body)
                    .map_err(|_| "账号服务返回了无效的登录状态。".to_owned())?;
                session.email = response.user.email.clone();
                let device_id = session.device_id.clone();
                self.stage_session(generation, session)?;
                Ok(AccountSnapshot::signed_in(response.user, device_id))
            }
            Err(_) => {
                self.operation.with_current(generation, || Ok(()))?;
                let response: RefreshResponse = self
                    .request(
                        "POST",
                        "v1/account/refresh",
                        Some(json!({ "refreshToken": session.refresh_token })),
                        None,
                    )
                    .await
                    .and_then(|body| {
                        serde_json::from_value(body).map_err(|_| "刷新登录状态失败。".to_owned())
                    })?;
                session.access_token = response.tokens.access_token;
                session.refresh_token = response.tokens.refresh_token;
                self.operation.with_current(generation, || Ok(()))?;
                let body = self
                    .request("GET", "v1/account/me", None, Some(&session.access_token))
                    .await?;
                let me: MeResponse = serde_json::from_value(body)
                    .map_err(|_| "账号服务返回了无效的用户信息。".to_owned())?;
                let device_id = session.device_id.clone();
                self.stage_session(generation, session)?;
                Ok(AccountSnapshot::signed_in(me.user, device_id))
            }
        }
    }

    pub async fn register(
        &self,
        email: String,
        password: String,
        generation: u64,
    ) -> Result<(), String> {
        self.begin_operation(generation)?;
        let _ = self
            .request(
                "POST",
                "v1/account/register",
                Some(json!({
                    "email": email,
                    "password": password,
                    "deviceId": self.device_id()?,
                    "platform": "windows",
                    "deviceName": "Windows 电脑"
                })),
                None,
            )
            .await?;
        self.operation.with_current(generation, || Ok(()))
    }

    pub async fn verify_email(&self, token: String) -> Result<(), String> {
        self.request(
            "POST",
            "v1/account/verify-email",
            Some(json!({ "token": token })),
            None,
        )
        .await
        .map(|_| ())
    }

    pub async fn resend_verification(&self, email: String, generation: u64) -> Result<(), String> {
        self.begin_operation(generation)?;
        self.request(
            "POST",
            "v1/account/verify-email/resend",
            Some(json!({ "email": email })),
            None,
        )
        .await?;
        self.operation.with_current(generation, || Ok(()))
    }

    pub async fn login(
        &self,
        email: String,
        password: String,
        generation: u64,
    ) -> Result<AccountSnapshot, String> {
        self.begin_operation(generation)?;
        if self.store.is_none() {
            return Err("当前 Windows 账户无法安全保存账号会话，本机模式仍可继续使用。".to_owned());
        }
        let device_id = self.device_id()?;
        let body = self
            .request(
                "POST",
                "v1/account/login",
                Some(json!({
                    "email": email,
                    "password": password,
                    "deviceId": device_id,
                    "platform": "windows",
                    "deviceName": "Windows 电脑"
                })),
                None,
            )
            .await?;
        let response: LoginResponse = serde_json::from_value(body)
            .map_err(|_| "账号服务返回了无效的登录结果。".to_owned())?;
        let session = AccountSession {
            user_id: response.user.id.clone(),
            email: response.user.email.clone(),
            device_id: device_id.clone(),
            access_token: response.tokens.access_token,
            refresh_token: response.tokens.refresh_token,
        };
        self.stage_session(generation, session)?;
        Ok(AccountSnapshot::signed_in(response.user, device_id))
    }

    pub async fn forgot_password(&self, email: String, generation: u64) -> Result<(), String> {
        self.begin_operation(generation)?;
        self.request(
            "POST",
            "v1/account/password/forgot",
            Some(json!({ "email": email })),
            None,
        )
        .await?;
        self.operation.with_current(generation, || Ok(()))
    }

    pub async fn logout(&self) -> Result<(), String> {
        // Clear locally before the network wait. A late logout response must
        // never erase a newer login that started while revocation was pending.
        let session = {
            let mut state = self
                .operation
                .state
                .lock()
                .map_err(|_| SUPERSEDED_OPERATION.to_owned())?;
            state.active = false;
            state.pending_session = None;
            state.previous_session = None;
            let Some(store) = self.store.as_ref() else {
                return Ok(());
            };
            let session = store.load().map_err(|error| error.to_string())?;
            store.clear().map_err(|error| error.to_string())?;
            session
        };
        if let Some(session) = session {
            let _ = self
                .request(
                    "POST",
                    "v1/account/logout",
                    Some(json!({})),
                    Some(&session.access_token),
                )
                .await;
        }
        Ok(())
    }

    fn device_id(&self) -> Result<String, String> {
        let identity_store = self
            .identity_store
            .as_ref()
            .ok_or_else(|| "本机身份存储不可用。".to_owned())?;
        let identity = identity_store
            .load_or_create(&mut OsRng)
            .map_err(|error| error.to_string())?;
        Ok(identity
            .device_id
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect())
    }

    async fn request(
        &self,
        method: &str,
        path: &str,
        body: Option<Value>,
        access_token: Option<&str>,
    ) -> Result<Value, String> {
        let url = format!(
            "{}/{}",
            self.base_url.trim_end_matches('/'),
            path.trim_start_matches('/')
        );
        let method = reqwest::Method::from_bytes(method.as_bytes())
            .map_err(|_| "账号请求方法无效。".to_owned())?;
        let client = self
            .client
            .as_ref()
            .ok_or_else(|| "账号网络模块暂时不可用，本机模式仍可继续使用。".to_owned())?;
        let mut request = client
            .request(method, url)
            .header("accept", "application/json");
        if let Some(body) = body {
            request = request.json(&body);
        }
        if let Some(token) = access_token {
            request = request.bearer_auth(token);
        }
        let response = request
            .send()
            .await
            .map_err(|_| "无法连接账号服务，请检查网络后重试。".to_owned())?;
        let status = response.status();
        let data = response
            .bytes()
            .await
            .map_err(|_| "账号服务响应读取失败，请稍后重试。".to_owned())?;
        let body = serde_json::from_slice::<Value>(&data).unwrap_or_else(|_| json!({}));
        if !status.is_success() {
            let message = serde_json::from_value::<ErrorResponse>(body.clone())
                .ok()
                .and_then(|error| error.message)
                .unwrap_or_else(|| "账号服务暂时不可用，请稍后重试。".to_owned());
            return Err(message);
        }
        Ok(body)
    }
}

impl AccountSnapshot {
    pub fn signed_out() -> Self {
        Self {
            signed_in: false,
            user: None,
            device_id: None,
        }
    }

    pub fn signed_in(user: AccountUser, device_id: String) -> Self {
        Self {
            signed_in: true,
            user: Some(user),
            device_id: Some(device_id),
        }
    }
}

fn account_base_url() -> String {
    static VALUE: OnceLock<String> = OnceLock::new();
    VALUE
        .get_or_init(|| {
            std::env::var("DESKLINK_ACCOUNT_URL")
                .unwrap_or_else(|_| "https://account.p2p.yxswy.com".to_owned())
        })
        .clone()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};
    use tokio::{
        io::{AsyncReadExt, AsyncWriteExt},
        net::TcpListener,
        sync::oneshot,
    };

    struct TestAccount {
        manager: AccountManager,
        directory: std::path::PathBuf,
    }

    impl TestAccount {
        fn new(base_url: String) -> Self {
            static NEXT: AtomicU64 = AtomicU64::new(0);
            let directory = std::env::temp_dir().join(format!(
                "desklink-account-owner-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            let manager = AccountManager {
                store: Some(WindowsAccountSessionStore::new(
                    directory.join("account.bin"),
                )),
                identity_store: Some(WindowsIdentityStore::new(directory.join("identity.bin"))),
                operation: AccountOperationOwner::default(),
                client: Some(
                    Client::builder()
                        .timeout(ACCOUNT_REQUEST_TIMEOUT)
                        .build()
                        .unwrap(),
                ),
                base_url,
            };
            Self { manager, directory }
        }

        fn saved_user(&self) -> Option<String> {
            self.manager
                .store
                .as_ref()
                .unwrap()
                .load()
                .unwrap()
                .map(|session| session.user_id.clone())
        }
    }

    impl Drop for TestAccount {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.directory);
        }
    }

    fn session(user: &str) -> AccountSession {
        AccountSession {
            user_id: user.to_owned(),
            email: format!("{user}@example.invalid"),
            device_id: "synthetic-device".to_owned(),
            access_token: format!("synthetic-access-{user}"),
            refresh_token: format!("synthetic-refresh-{user}"),
        }
    }

    fn login_body(user: &str) -> Value {
        json!({"user": {"id":user,"email":format!("{user}@example.invalid"),"emailVerified":true},
            "tokens":{"accessToken":"synthetic-access","refreshToken":"synthetic-refresh"}})
    }

    async fn delayed_response(
        body: Value,
    ) -> (
        String,
        oneshot::Receiver<()>,
        oneshot::Sender<()>,
        tokio::task::JoinHandle<()>,
    ) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let (received_tx, received_rx) = oneshot::channel();
        let (release_tx, release_rx) = oneshot::channel();
        let task = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut request = Vec::new();
            let mut buffer = [0; 1024];
            loop {
                let count = socket.read(&mut buffer).await.unwrap();
                assert!(count > 0);
                request.extend_from_slice(&buffer[..count]);
                if let Some(end) = request.windows(4).position(|bytes| bytes == b"\r\n\r\n") {
                    let headers = String::from_utf8_lossy(&request[..end]).to_lowercase();
                    let content_length = headers
                        .lines()
                        .find_map(|line| line.strip_prefix("content-length: "))
                        .map(|value| value.parse::<usize>().unwrap())
                        .unwrap_or(0);
                    if request.len() >= end + 4 + content_length {
                        break;
                    }
                }
            }
            received_tx.send(()).unwrap();
            release_rx.await.unwrap();
            let body = body.to_string();
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                body.len(),
                body
            );
            socket.write_all(response.as_bytes()).await.unwrap();
        });
        (url, received_rx, release_tx, task)
    }

    #[tokio::test]
    async fn delayed_login_cannot_persist_after_view_cancellation() {
        let (url, received, release, server) = delayed_response(login_body("new-user")).await;
        let fixture = TestAccount::new(url);
        fixture
            .manager
            .store
            .as_ref()
            .unwrap()
            .save(&session("prior-user"))
            .unwrap();
        let manager = fixture.manager.clone();
        let login = tokio::spawn(async move {
            manager
                .login(
                    "new@example.invalid".to_owned(),
                    "synthetic-password".to_owned(),
                    10,
                )
                .await
        });
        received.await.unwrap();
        fixture.manager.cancel_operation(11).unwrap();
        release.send(()).unwrap();
        assert!(login.await.unwrap().is_err());
        server.await.unwrap();
        assert_eq!(fixture.saved_user().as_deref(), Some("prior-user"));
        assert!(fixture.manager.accept_operation(10).is_err());
    }

    #[tokio::test]
    async fn login_is_pending_until_accepted_and_cancel_restores_prior_session() {
        let (url, received, release, server) = delayed_response(login_body("new-user")).await;
        let fixture = TestAccount::new(url);
        fixture
            .manager
            .store
            .as_ref()
            .unwrap()
            .save(&session("prior-user"))
            .unwrap();
        let manager = fixture.manager.clone();
        let login = tokio::spawn(async move {
            manager
                .login(
                    "new@example.invalid".to_owned(),
                    "synthetic-password".to_owned(),
                    20,
                )
                .await
        });
        received.await.unwrap();
        release.send(()).unwrap();
        assert!(login.await.unwrap().unwrap().signed_in);
        server.await.unwrap();
        assert_eq!(fixture.saved_user().as_deref(), Some("prior-user"));
        fixture.manager.accept_operation(20).unwrap();
        assert_eq!(fixture.saved_user().as_deref(), Some("new-user"));
        fixture.manager.cancel_operation(21).unwrap();
        assert_eq!(fixture.saved_user().as_deref(), Some("prior-user"));
    }

    #[tokio::test]
    async fn delayed_restore_cannot_write_or_rearm_host_after_local_mode() {
        let (url, received, release, server) = delayed_response(json!({"user":{"id":"prior-user","email":"updated@example.invalid","emailVerified":true}})).await;
        let fixture = TestAccount::new(url);
        fixture
            .manager
            .store
            .as_ref()
            .unwrap()
            .save(&session("prior-user"))
            .unwrap();
        let manager = fixture.manager.clone();
        let restore = tokio::spawn(async move { manager.restore(30).await });
        received.await.unwrap();
        fixture.manager.cancel_operation(31).unwrap();
        release.send(()).unwrap();
        assert!(restore.await.unwrap().is_err());
        server.await.unwrap();
        let stored = fixture
            .manager
            .store
            .as_ref()
            .unwrap()
            .load()
            .unwrap()
            .unwrap();
        assert_eq!(stored.email, "prior-user@example.invalid");
        let mut rearmed = false;
        assert!(
            fixture
                .manager
                .operation_owner()
                .with_current(30, || {
                    rearmed = true;
                    Ok(())
                })
                .is_err()
        );
        assert!(!rearmed);
    }

    #[test]
    fn out_of_order_ipc_and_duplicate_generation_cannot_claim_ownership() {
        let fixture = TestAccount::new("http://127.0.0.1:1".to_owned());
        fixture.manager.begin_operation(50).unwrap();
        fixture.manager.cancel_operation(49).unwrap();
        assert!(
            fixture
                .manager
                .operation_owner()
                .with_current(50, || Ok(()))
                .is_ok()
        );
        assert!(fixture.manager.begin_operation(50).is_err());
        assert!(fixture.manager.begin_operation(48).is_err());
        fixture.manager.cancel_operation(51).unwrap();
        assert!(fixture.manager.begin_operation(50).is_err());
        assert!(fixture.manager.stage_session(50, session("late")).is_err());
    }

    #[test]
    fn accepted_workspace_keeps_session_and_unaccepted_first_login_is_removed() {
        let fixture = TestAccount::new("http://127.0.0.1:1".to_owned());
        fixture.manager.begin_operation(60).unwrap();
        fixture.manager.stage_session(60, session("first")).unwrap();
        fixture.manager.accept_operation(60).unwrap();
        fixture.manager.cancel_operation(61).unwrap();
        assert!(fixture.saved_user().is_none());
        fixture.manager.begin_operation(62).unwrap();
        fixture
            .manager
            .stage_session(62, session("accepted"))
            .unwrap();
        fixture.manager.accept_operation(62).unwrap();
        fixture.manager.finish_operation(62).unwrap();
        fixture.manager.cancel_operation(63).unwrap();
        assert_eq!(fixture.saved_user().as_deref(), Some("accepted"));
    }
    #[tokio::test]
    async fn late_first_login_cannot_replace_a_completed_retry() {
        let (old_url, old_received, old_release, old_server) =
            delayed_response(login_body("old-attempt")).await;
        let (new_url, new_received, new_release, new_server) =
            delayed_response(login_body("new-attempt")).await;
        let fixture = TestAccount::new(old_url);
        let old_manager = fixture.manager.clone();
        let old_login = tokio::spawn(async move {
            old_manager
                .login(
                    "old@example.invalid".to_owned(),
                    "synthetic-password".to_owned(),
                    70,
                )
                .await
        });
        old_received.await.unwrap();
        let mut new_manager = fixture.manager.clone();
        new_manager.base_url = new_url;
        let new_login = tokio::spawn(async move {
            new_manager
                .login(
                    "new@example.invalid".to_owned(),
                    "synthetic-password".to_owned(),
                    71,
                )
                .await
        });
        new_received.await.unwrap();
        new_release.send(()).unwrap();
        assert!(new_login.await.unwrap().unwrap().signed_in);
        fixture.manager.accept_operation(71).unwrap();
        fixture.manager.finish_operation(71).unwrap();
        old_release.send(()).unwrap();
        assert!(old_login.await.unwrap().is_err());
        old_server.await.unwrap();
        new_server.await.unwrap();
        assert_eq!(fixture.saved_user().as_deref(), Some("new-attempt"));
    }

    #[tokio::test]
    async fn late_logout_response_cannot_clear_a_newly_accepted_session() {
        let (url, received, release, server) = delayed_response(json!({})).await;
        let fixture = TestAccount::new(url);
        fixture
            .manager
            .store
            .as_ref()
            .unwrap()
            .save(&session("previous"))
            .unwrap();
        let manager = fixture.manager.clone();
        let logout = tokio::spawn(async move { manager.logout().await });
        received.await.unwrap();
        assert!(fixture.saved_user().is_none());
        fixture.manager.begin_operation(80).unwrap();
        fixture
            .manager
            .stage_session(80, session("new-login"))
            .unwrap();
        fixture.manager.accept_operation(80).unwrap();
        fixture.manager.finish_operation(80).unwrap();
        release.send(()).unwrap();
        logout.await.unwrap().unwrap();
        server.await.unwrap();
        assert_eq!(fixture.saved_user().as_deref(), Some("new-login"));
    }
    #[tokio::test]
    async fn cancelled_acceptance_cannot_spawn_after_waiting_for_host_shutdown() {
        use std::sync::atomic::AtomicBool;
        let fixture = TestAccount::new("http://127.0.0.1:1".to_owned());
        fixture.manager.begin_operation(90).unwrap();
        fixture
            .manager
            .stage_session(90, session("pending-host"))
            .unwrap();
        fixture.manager.accept_operation(90).unwrap();
        let owner = fixture.manager.operation_owner();
        let (stopping_tx, stopping_rx) = oneshot::channel();
        let (stopped_tx, stopped_rx) = oneshot::channel();
        let spawned = Arc::new(AtomicBool::new(false));
        let did_spawn = spawned.clone();
        let restart = tokio::spawn(async move {
            owner
                .after_wait(
                    90,
                    async {
                        stopping_tx.send(()).unwrap();
                        stopped_rx.await.unwrap();
                    },
                    || {
                        did_spawn.store(true, Ordering::Release);
                        Ok(())
                    },
                )
                .await
        });
        stopping_rx.await.unwrap();
        fixture.manager.cancel_operation(91).unwrap();
        stopped_tx.send(()).unwrap();
        assert!(restart.await.unwrap().is_err());
        assert!(!spawned.load(Ordering::Acquire));
        assert!(fixture.saved_user().is_none());
    }
    #[test]
    fn failed_rollback_retains_the_prior_session_for_a_later_safe_retry() {
        let fixture = TestAccount::new("http://127.0.0.1:1".to_owned());
        fixture
            .manager
            .store
            .as_ref()
            .unwrap()
            .save(&session("prior-user"))
            .unwrap();
        fixture.manager.begin_operation(100).unwrap();
        fixture
            .manager
            .stage_session(100, session("unaccepted-user"))
            .unwrap();
        fixture.manager.accept_operation(100).unwrap();
        // Block the store's actual atomic replacement file using an owned
        // synthetic directory, without changing real account permissions.
        let temporary = fixture.directory.join("account.tmp");
        std::fs::create_dir(&temporary).unwrap();
        assert!(fixture.manager.cancel_operation(101).is_err());
        assert_eq!(fixture.saved_user().as_deref(), Some("unaccepted-user"));
        assert!(
            fixture
                .manager
                .operation
                .state
                .lock()
                .unwrap()
                .previous_session
                .is_some()
        );
        assert!(
            fixture
                .manager
                .operation_owner()
                .with_current(100, || Ok(()))
                .is_err()
        );
        std::fs::remove_dir(&temporary).unwrap();
        fixture.manager.cancel_operation(102).unwrap();
        assert_eq!(fixture.saved_user().as_deref(), Some("prior-user"));
        assert!(
            fixture
                .manager
                .operation
                .state
                .lock()
                .unwrap()
                .previous_session
                .is_none()
        );
    }
}
