use async_trait::async_trait;
use chrono::{DateTime, Utc};

use crate::error::Result;
use crate::types::SocialPost;

#[async_trait]
pub trait SocialProvider: Send + Sync {
    fn name(&self) -> &str;
    async fn fetch(&self, accounts: &[String], since: DateTime<Utc>) -> Result<Vec<SocialPost>>;
}
