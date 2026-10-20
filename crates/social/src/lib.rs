use async_trait::async_trait;
use chrono::{DateTime, Utc};
use engine::{Result, SocialPost, SocialProvider};

pub struct NullSocialProvider;

#[async_trait]
impl SocialProvider for NullSocialProvider {
    fn name(&self) -> &str {
        "none"
    }

    async fn fetch(&self, _accounts: &[String], _since: DateTime<Utc>) -> Result<Vec<SocialPost>> {
        Ok(Vec::new())
    }
}

pub fn build_provider(name: &str) -> Box<dyn SocialProvider> {
    match name.to_ascii_lowercase().as_str() {
        "" | "none" | "null" => Box::new(NullSocialProvider),
        other => {
            tracing::warn!(
                provider = other,
                "unknown social provider configured; continuing with no social source"
            );
            Box::new(NullSocialProvider)
        }
    }
}
