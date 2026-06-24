use crate::proxy::providers::ollama_cloud::OllamaModel;
use crate::proxy::providers::ollama_cloud_auth::{
    OllamaCloudAccountManager, OllamaCloudManagedAccount, OllamaCloudStatus,
};
use std::sync::Arc;
use tauri::State;
use tokio::sync::RwLock;

pub struct OllamaCloudState(pub Arc<RwLock<OllamaCloudAccountManager>>);

#[tauri::command(rename_all = "camelCase")]
pub async fn ollama_cloud_import_api_key(
    api_key: String,
    label: Option<String>,
    state: State<'_, OllamaCloudState>,
) -> Result<OllamaCloudManagedAccount, String> {
    state
        .0
        .read()
        .await
        .import_api_key(api_key, label)
        .await
        .map_err(|e| e.to_string())
}

#[tauri::command(rename_all = "camelCase")]
pub async fn ollama_cloud_list_accounts(
    state: State<'_, OllamaCloudState>,
) -> Result<Vec<OllamaCloudManagedAccount>, String> {
    Ok(state.0.read().await.list_accounts().await)
}

#[tauri::command(rename_all = "camelCase")]
pub async fn ollama_cloud_status(
    state: State<'_, OllamaCloudState>,
) -> Result<OllamaCloudStatus, String> {
    Ok(state.0.read().await.get_status().await)
}

#[tauri::command(rename_all = "camelCase")]
pub async fn ollama_cloud_remove_account(
    account_id: String,
    state: State<'_, OllamaCloudState>,
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
pub async fn ollama_cloud_set_default_account(
    account_id: String,
    state: State<'_, OllamaCloudState>,
) -> Result<(), String> {
    state
        .0
        .read()
        .await
        .set_default_account(&account_id)
        .await
        .map_err(|e| e.to_string())
}

#[tauri::command(rename_all = "camelCase")]
pub async fn ollama_cloud_test_connection(
    api_key: String,
    state: State<'_, OllamaCloudState>,
) -> Result<Vec<OllamaModel>, String> {
    state
        .0
        .read()
        .await
        .test_connection(&api_key)
        .await
        .map_err(|e| e.to_string())
}

#[tauri::command(rename_all = "camelCase")]
pub async fn ollama_cloud_list_models(
    account_id: Option<String>,
    state: State<'_, OllamaCloudState>,
) -> Result<Vec<OllamaModel>, String> {
    state
        .0
        .read()
        .await
        .list_models(account_id)
        .await
        .map_err(|e| e.to_string())
}

#[tauri::command(rename_all = "camelCase")]
pub async fn ollama_cloud_list_tags(
    account_id: Option<String>,
    state: State<'_, OllamaCloudState>,
) -> Result<Vec<OllamaModel>, String> {
    state
        .0
        .read()
        .await
        .list_tags(account_id)
        .await
        .map_err(|e| e.to_string())
}
