//! DeepSeek Account Tauri commands.

use crate::proxy::providers::deepseek_account_auth::{
    DeepSeekAccountManager, DeepSeekAccountStatus, DeepSeekManagedAccount,
};
use std::sync::Arc;
use tauri::State;
use tokio::sync::RwLock;

pub struct DeepSeekAccountState(pub Arc<RwLock<DeepSeekAccountManager>>);

#[tauri::command(rename_all = "camelCase")]
pub async fn deepseek_account_add(
    email: Option<String>,
    mobile: Option<String>,
    password: String,
    state: State<'_, DeepSeekAccountState>,
) -> Result<DeepSeekManagedAccount, String> {
    state
        .0
        .read()
        .await
        .add_account(email, mobile, password)
        .await
        .map_err(|e| e.to_string())
}

#[tauri::command(rename_all = "camelCase")]
pub async fn deepseek_account_list(
    state: State<'_, DeepSeekAccountState>,
) -> Result<Vec<DeepSeekManagedAccount>, String> {
    Ok(state.0.read().await.list_accounts().await)
}

#[tauri::command(rename_all = "camelCase")]
pub async fn deepseek_account_status(
    state: State<'_, DeepSeekAccountState>,
) -> Result<DeepSeekAccountStatus, String> {
    Ok(state.0.read().await.get_status().await)
}

#[tauri::command(rename_all = "camelCase")]
pub async fn deepseek_account_remove(
    account_id: String,
    state: State<'_, DeepSeekAccountState>,
) -> Result<(), String> {
    state
        .0
        .read()
        .await
        .remove_account(&account_id)
        .await
        .map_err(|e| e.to_string())
}

#[tauri::command(rename_all = "camelCase")]
pub async fn deepseek_account_set_default(
    account_id: String,
    state: State<'_, DeepSeekAccountState>,
) -> Result<(), String> {
    state
        .0
        .read()
        .await
        .set_default_account(&account_id)
        .await
        .map_err(|e| e.to_string())
}
