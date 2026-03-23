use std::env;

#[derive(Clone, Debug)]
pub struct AppConfig {
    pub app_host: String,
    pub app_port: u16,
    pub database_url: String,
    pub allowed_origin: String,
    pub cookie_secret: String,
}

impl AppConfig {
    pub fn from_env() -> Self {
        Self {
            app_host: env::var("APP_HOST").unwrap_or_else(|_| "127.0.0.1".to_string()),
            app_port: env::var("APP_PORT")
                .ok()
                .and_then(|value| value.parse::<u16>().ok())
                .unwrap_or(3001),
            database_url: env::var("DATABASE_URL")
                .unwrap_or_else(|_| "sqlite://./dnd.sqlite?mode=rwc".to_string()),
            allowed_origin: env::var("ALLOWED_ORIGIN")
                .unwrap_or_else(|_| "http://localhost:5173".to_string()),
            cookie_secret: env::var("COOKIE_SECRET")
                .unwrap_or_else(|_| "dev-cookie-secret-dev-cookie-secret".to_string()),
        }
    }

    pub fn bind_address(&self) -> String {
        format!("{}:{}", self.app_host, self.app_port)
    }
}
