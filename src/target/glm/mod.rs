pub mod accounts;
pub mod admin;
pub mod anthropic;
pub mod api;
pub mod quota;

pub const PROVIDER_NAME: &str = "glm";
pub const DEFAULT_API_USAGE_OPENAI_BASE_URL: &str = "https://api.z.ai/api/paas/v4";
pub const DEFAULT_SUBSCRIPTION_OPENAI_BASE_URL: &str = "https://api.z.ai/api/coding/paas/v4";
pub const DEFAULT_OPENAI_BASE_URL: &str = DEFAULT_API_USAGE_OPENAI_BASE_URL;
pub const DEFAULT_ANTHROPIC_BASE_URL: &str = "https://api.z.ai/api/anthropic";
