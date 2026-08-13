use std::sync::OnceLock;
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

#[derive(Clone)]
pub struct AccountManager {
    store: Option<WindowsAccountSessionStore>,
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
            client,
            base_url: account_base_url(),
        }
    }

    pub async fn restore(&self) -> Result<AccountSnapshot, String> {
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
                store.save(&session).map_err(|error| error.to_string())?;
                Ok(AccountSnapshot::signed_in(
                    response.user,
                    session.device_id.clone(),
                ))
            }
            Err(_) => {
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
                store.save(&session).map_err(|error| error.to_string())?;
                let body = self
                    .request("GET", "v1/account/me", None, Some(&session.access_token))
                    .await?;
                let me: MeResponse = serde_json::from_value(body)
                    .map_err(|_| "账号服务返回了无效的用户信息。".to_owned())?;
                Ok(AccountSnapshot::signed_in(
                    me.user,
                    session.device_id.clone(),
                ))
            }
        }
    }

    pub async fn register(&self, email: String, password: String) -> Result<(), String> {
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
        Ok(())
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

    pub async fn resend_verification(&self, email: String) -> Result<(), String> {
        self.request(
            "POST",
            "v1/account/verify-email/resend",
            Some(json!({ "email": email })),
            None,
        )
        .await
        .map(|_| ())
    }

    pub async fn login(&self, email: String, password: String) -> Result<AccountSnapshot, String> {
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
        self.store
            .as_ref()
            .ok_or_else(|| "登录成功，但当前 Windows 账户无法安全保存账号会话。".to_owned())?
            .save(&session)
            .map_err(|error| error.to_string())?;
        Ok(AccountSnapshot::signed_in(response.user, device_id))
    }

    pub async fn forgot_password(&self, email: String) -> Result<(), String> {
        self.request(
            "POST",
            "v1/account/password/forgot",
            Some(json!({ "email": email })),
            None,
        )
        .await
        .map(|_| ())
    }

    pub async fn logout(&self) -> Result<(), String> {
        let Some(store) = self.store.as_ref() else {
            return Ok(());
        };
        if let Some(session) = store.load().map_err(|error| error.to_string())? {
            let _ = self
                .request(
                    "POST",
                    "v1/account/logout",
                    Some(json!({})),
                    Some(&session.access_token),
                )
                .await;
        }
        store.clear().map_err(|error| error.to_string())?;
        Ok(())
    }

    fn device_id(&self) -> Result<String, String> {
        let identity_store =
            WindowsIdentityStore::for_current_user().map_err(|error| error.to_string())?;
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
