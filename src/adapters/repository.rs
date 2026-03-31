use std::{collections::HashMap, path::PathBuf};

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use tokio::{fs, sync::RwLock};

use crate::{
    domain::{ConversationBinding, ConversationKey},
    error::BridgeError,
    ports::ConversationRepository,
};

#[derive(Default)]
pub struct InMemoryConversationRepository {
    entries: RwLock<HashMap<String, ConversationBinding>>,
}

#[async_trait]
impl ConversationRepository for InMemoryConversationRepository {
    async fn get(&self, key: &ConversationKey) -> Result<Option<ConversationBinding>, BridgeError> {
        Ok(self.entries.read().await.get(&key.storage_key()).cloned())
    }

    async fn put(
        &self,
        key: &ConversationKey,
        binding: &ConversationBinding,
    ) -> Result<(), BridgeError> {
        self.entries
            .write()
            .await
            .insert(key.storage_key(), binding.clone());
        Ok(())
    }
}

pub struct FileConversationRepository {
    path: PathBuf,
    cache: RwLock<Option<HashMap<String, ConversationBinding>>>,
}

impl FileConversationRepository {
    pub fn new(path: PathBuf) -> Self {
        Self {
            path,
            cache: RwLock::new(None),
        }
    }

    async fn load_map(&self) -> Result<HashMap<String, ConversationBinding>, BridgeError> {
        {
            let cache = self.cache.read().await;
            if let Some(entries) = &*cache {
                return Ok(entries.clone());
            }
        }

        let loaded = match fs::read_to_string(&self.path).await {
            Ok(content) => serde_json::from_str::<PersistedConversations>(&content)
                .map(|value| value.entries)
                .map_err(|error| {
                    BridgeError::Persistence(format!(
                        "failed to parse {}: {error}",
                        self.path.display()
                    ))
                })?,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => HashMap::new(),
            Err(error) => {
                return Err(BridgeError::Persistence(format!(
                    "failed to read {}: {error}",
                    self.path.display()
                )));
            }
        };

        *self.cache.write().await = Some(loaded.clone());
        Ok(loaded)
    }

    async fn persist_map(
        &self,
        entries: &HashMap<String, ConversationBinding>,
    ) -> Result<(), BridgeError> {
        if let Some(parent) = self.path.parent() {
            fs::create_dir_all(parent).await.map_err(|error| {
                BridgeError::Persistence(format!("failed to create {}: {error}", parent.display()))
            })?;
        }

        let content = serde_json::to_string_pretty(&PersistedConversations {
            entries: entries.clone(),
        })
        .map_err(|error| BridgeError::Persistence(format!("failed to serialize state: {error}")))?;

        fs::write(&self.path, content).await.map_err(|error| {
            BridgeError::Persistence(format!("failed to write {}: {error}", self.path.display()))
        })?;

        *self.cache.write().await = Some(entries.clone());
        Ok(())
    }
}

#[async_trait]
impl ConversationRepository for FileConversationRepository {
    async fn get(&self, key: &ConversationKey) -> Result<Option<ConversationBinding>, BridgeError> {
        Ok(self.load_map().await?.get(&key.storage_key()).cloned())
    }

    async fn put(
        &self,
        key: &ConversationKey,
        binding: &ConversationBinding,
    ) -> Result<(), BridgeError> {
        let mut entries = self.load_map().await?;
        entries.insert(key.storage_key(), binding.clone());
        self.persist_map(&entries).await
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct PersistedConversations {
    entries: HashMap<String, ConversationBinding>,
}

#[cfg(test)]
mod tests {
    use tempfile::tempdir;

    use crate::{
        adapters::repository::{FileConversationRepository, InMemoryConversationRepository},
        domain::{ConversationBinding, ConversationKey, PermissionMode},
        ports::ConversationRepository,
    };

    fn key() -> ConversationKey {
        ConversationKey {
            tenant_key: "tenant".to_string(),
            chat_id: "chat".to_string(),
            user_open_id: "user".to_string(),
            thread_id: None,
        }
    }

    fn binding() -> ConversationBinding {
        ConversationBinding {
            cwd: std::path::PathBuf::from("/repo"),
            agent: "codex".to_string(),
            session_name: Some("backend".to_string()),
            permission_mode: PermissionMode::ApproveReads,
        }
    }

    #[tokio::test]
    async fn in_memory_repository_round_trips() {
        let repository = InMemoryConversationRepository::default();
        repository.put(&key(), &binding()).await.unwrap();
        assert_eq!(repository.get(&key()).await.unwrap(), Some(binding()));
    }

    #[tokio::test]
    async fn file_repository_persists_to_disk() {
        let temp = tempdir().unwrap();
        let path = temp.path().join("conversations.json");
        let repository = FileConversationRepository::new(path.clone());
        repository.put(&key(), &binding()).await.unwrap();

        let restored = FileConversationRepository::new(path);
        assert_eq!(restored.get(&key()).await.unwrap(), Some(binding()));
    }
}
