use crate::livestream::StreamInfo;

use anyhow::{Context, Result, anyhow, bail};
use redis::{AsyncTypedCommands, ToRedisArgs, ToSingleRedisArg, aio::ConnectionManager};

#[derive(Clone, Debug)]
pub struct RedisClient {
    conn: ConnectionManager,
}

impl RedisClient {
    pub async fn create(address: &str) -> Result<Self> {
        let client = redis::Client::open(address)?;
        let conn = ConnectionManager::new(client).await?;
        Ok(Self { conn })
    }

    fn stream_info_key(live_id: &str) -> String {
        format!("livestream:streams:{live_id}")
    }

    pub async fn get_live_ids(&self) -> Result<Vec<String>> {
        let mut conn = self.conn.clone();
        let keys: Vec<String> = conn.keys("livestream:streams:").await?;
        let live_ids = keys
            .iter()
            .map(|key| key.trim_start_matches("livestream:streams:").to_string())
            .collect();
        Ok(live_ids)
    }

    pub async fn cache_stream_info(&self, stream_info: StreamInfo) -> Result<()> {
        let key = Self::stream_info_key(stream_info.live_id());
        let mut conn = self.conn.clone();

        if conn.exists(&key).await? {
            bail!("Duplicate stream info id");
        }

        conn.set(key, stream_info.clone()).await?;
        Ok(())
    }

    pub async fn find_stream_info(&self, live_id: &str) -> Result<Option<StreamInfo>> {
        let key = Self::stream_info_key(live_id);
        let mut conn = self.conn.clone();

        let info: Option<StreamInfo> = conn
            .get(key)
            .await?
            .map(|json| serde_json::from_str(&json).unwrap());
        Ok(info)
    }

    pub async fn remove_stream_info(&self, live_id: &str) -> Result<()> {
        let key = Self::stream_info_key(live_id);
        let mut conn = self.conn.clone();

        if conn.del(key).await? != 1 {
            bail!("Errors occurred when removing stream info {live_id} from the cache")
        }
        Ok(())
    }

    pub async fn publish_stream_event(&self, live_id: &str, event: &str) -> Result<()> {
        let mut conn = self.conn.clone();
        let channel = format!("livestream:events:{live_id}");
        conn.publish(channel, event).await?;
        Ok(())
    }
}

impl ToRedisArgs for StreamInfo {
    fn write_redis_args<W>(&self, out: &mut W)
    where
        W: ?Sized + redis::RedisWrite,
    {
        let json = serde_json::to_string(self)
            .with_context(|| anyhow!("Failed to serialize to stream info {}", self.live_id()))
            .unwrap();

        out.write_arg(json.as_bytes());
    }
}

impl ToSingleRedisArg for StreamInfo {}
