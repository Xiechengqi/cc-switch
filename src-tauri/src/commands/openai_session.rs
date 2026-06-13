//! OpenAI Official session provider commands.

use crate::proxy::providers::openai_session_auth::{
    OpenAISessionImportOutcome, OpenAISessionManager, OpenAISessionStatus,
};
use std::sync::Arc;
use tauri::State;
use tokio::sync::RwLock;

pub struct OpenAISessionState(pub Arc<RwLock<OpenAISessionManager>>);

#[tauri::command(rename_all = "camelCase")]
pub async fn openai_session_import(
    session_json: String,
    state: State<'_, OpenAISessionState>,
) -> Result<OpenAISessionImportOutcome, String> {
    let manager = state.0.read().await;
    manager
        .import_session_json(&session_json)
        .await
        .map_err(|e| e.to_string())
}

#[tauri::command(rename_all = "camelCase")]
pub async fn openai_session_status(
    state: State<'_, OpenAISessionState>,
) -> Result<OpenAISessionStatus, String> {
    let manager = state.0.read().await;
    Ok(manager.get_status().await)
}

#[tauri::command(rename_all = "camelCase")]
pub async fn openai_session_remove(
    account_id: String,
    state: State<'_, OpenAISessionState>,
) -> Result<(), String> {
    let manager = state.0.read().await;
    manager
        .remove_account(&account_id)
        .await
        .map_err(|e| e.to_string())
}

#[tauri::command(rename_all = "camelCase")]
pub async fn openai_session_set_default(
    account_id: String,
    state: State<'_, OpenAISessionState>,
) -> Result<(), String> {
    let manager = state.0.read().await;
    manager
        .set_default_account(&account_id)
        .await
        .map_err(|e| e.to_string())
}

#[tauri::command(rename_all = "camelCase")]
pub async fn openai_session_clear(state: State<'_, OpenAISessionState>) -> Result<(), String> {
    let manager = state.0.read().await;
    manager.clear_auth().await.map_err(|e| e.to_string())
}
